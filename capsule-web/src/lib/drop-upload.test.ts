// Guest-drop uploader: chunking, checksums, progress, and failure
// classification, driven against a mocked fetch. No crypto here — the seal is exercised by the
// cross-language KAT; this pins the HTTP protocol `capsule-server`'s drop routes require, and it
// is what caught the paths and the body going stale when the Salvo tree retired.

import { describe, expect, test } from 'bun:test';

import type { SealedDropWire } from './drop-seal';
import {
    DropUploadError,
    MAX_CHUNK_WAITS,
    MAX_STATUS_POLLS,
    MAX_WAIT_MS,
    retryDelayMs,
    STATUS_POLL_MAX_MS,
    sha256Hex,
    statusPollDelayMs,
    uploadDrop,
} from './drop-upload';

const CHUNK_SIZE = 1 << 20; // must match drop-upload.ts

/** A sealed drop with a synthetic ciphertext of `size` bytes and a throwaway descriptor. */
function fakeSealed(size: number): SealedDropWire {
    const ciphertext = new Uint8Array(size);
    for (let i = 0; i < size; i++) ciphertext[i] = i % 251;
    return {
        size,
        descriptor: {
            content_type: 'image/jpeg',
            plaintext_size: size,
            chunk_size: 65536,
            nonce_prefix: '00112233445566',
            ciphertext_hash: 'a'.repeat(64),
            kem_ct: 'AAAA',
            suggested_filename: 'photo.jpg',
        },
        ciphertext,
    };
}

interface Call {
    url: string;
    method: string;
    headers: Headers;
    body: BodyInit | null | undefined;
}

/** A recording mock fetch driven by a per-request handler. */
function mockFetch(handler: (call: Call) => Response) {
    const calls: Call[] = [];
    const impl = ((input: RequestInfo | URL, init?: RequestInit) => {
        const call: Call = {
            url: String(input),
            method: init?.method ?? 'GET',
            headers: new Headers(init?.headers),
            body: init?.body,
        };
        calls.push(call);
        return Promise.resolve(handler(call));
    }) as typeof fetch;
    return { impl, calls };
}

const created = () =>
    new Response(
        JSON.stringify({
            upload_id: 'upload-123',
            suggested_chunk_size: 1 << 20,
        }),
        {
            status: 201,
            headers: { 'Content-Type': 'application/json' },
        },
    );

const chunkOk = (newOffset: number) =>
    new Response(null, {
        status: 200,
        headers: { 'X-Capsule-Offset': String(newOffset) },
    });

describe('uploadDrop — chunking & progress', () => {
    test('opens a session then streams aligned chunks with per-chunk checksums', async () => {
        const sealed = fakeSealed(CHUNK_SIZE + 100); // → two chunks: 1 MiB (aligned) + 100 (final)
        const progress: number[] = [];
        let offset = 0;
        const { impl, calls } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            // Chunk: acknowledge by advancing the offset by the body length.
            const len = (call.body as Uint8Array).length;
            offset += len;
            return chunkOk(offset);
        });

        await uploadDrop({
            base: '',
            opaqueId: 'opaque-abc',
            sealed,
            onProgress: (f) => progress.push(f),
            fetchImpl: impl,
        });

        // One create + two chunk PATCHes.
        expect(calls[0].method).toBe('POST');
        expect(calls[0].url).toBe('/d/opaque-abc');
        const patches = calls.filter((c) => c.method === 'PATCH');
        expect(patches.length).toBe(2);

        // First chunk: offset 0, exactly 1 MiB (4 KiB-aligned), octet-stream, correct checksum.
        expect(patches[0].url).toBe('/d/opaque-abc/upload-123');
        expect(patches[0].headers.get('Content-Type')).toBe(
            'application/octet-stream',
        );
        expect(patches[0].headers.get('X-Capsule-Offset')).toBe('0');
        const firstBody = patches[0].body as Uint8Array;
        expect(firstBody.length).toBe(CHUNK_SIZE);
        expect(firstBody.length % 4096).toBe(0);
        expect(patches[0].headers.get('X-Capsule-Checksum')).toBe(
            await sha256Hex(firstBody),
        );

        // Second (final) chunk: offset 1 MiB, the 100-byte remainder.
        expect(patches[1].headers.get('X-Capsule-Offset')).toBe(
            String(CHUNK_SIZE),
        );
        expect((patches[1].body as Uint8Array).length).toBe(100);

        // Progress ends at 1 after the final chunk.
        expect(progress.at(-1)).toBe(1);
        expect(progress.length).toBe(2);
    });

    test('sends the declaration the server asks for, with the proof only when there is one', async () => {
        const { impl, calls } = mockFetch((call) =>
            call.method === 'POST' ? created() : chunkOk(fakeSealed(10).size),
        );
        await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            passphraseProof: 'deadbeef',
            fetchImpl: impl,
        });
        const body = JSON.parse(calls[0].body as string);
        expect(body.passphrase_proof).toBe('deadbeef');
        expect(body.size).toBe(10);
        expect(body.content_type).toBe('image/jpeg');
        expect(body.ciphertext_hash).toBe('a'.repeat(64));
        expect(body.kem_ct).toBe('AAAA');
        expect(body.suggested_filename).toBe('photo.jpg');
        expect(body.descriptor).toBeUndefined();

        const { impl: impl2, calls: calls2 } = mockFetch((call) =>
            call.method === 'POST' ? created() : chunkOk(10),
        );
        await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl2,
        });
        // Absent, not present-null: the server's body is strict, and a present-null is a value
        // it would have to decide what to do with.
        expect('passphrase_proof' in JSON.parse(calls2[0].body as string)).toBe(
            false,
        );
    });
});

describe('uploadDrop — failure classification', () => {
    const coded = (status: number, code: string) =>
        new Response(JSON.stringify({ code, detail: 'x' }), {
            status,
            headers: { 'Content-Type': 'application/json' },
        });

    async function expectCreateFailure(
        response: Response,
        expected: string,
    ): Promise<void> {
        const { impl } = mockFetch(() => response);
        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
        }).catch((e) => e);
        expect(err).toBeInstanceOf(DropUploadError);
        expect((err as DropUploadError).code).toBe(expected);
    }

    test('a bare 404 is the indistinguishable "unavailable"', async () => {
        await expectCreateFailure(
            new Response(null, { status: 404 }),
            'unavailable',
        );
    });

    test('429 → rate_limited, 413 → too_large', async () => {
        await expectCreateFailure(
            new Response(null, { status: 429 }),
            'rate_limited',
        );
        await expectCreateFailure(
            new Response(null, { status: 413 }),
            'too_large',
        );
    });

    test('coded rejections map to their failure class', async () => {
        await expectCreateFailure(coded(403, 'error.quota.exceeded'), 'quota');
        await expectCreateFailure(
            coded(403, 'error.quota.grace_locked'),
            'quota',
        );
        await expectCreateFailure(coded(403, 'error.quota.suspended'), 'quota');
        await expectCreateFailure(
            coded(403, 'error.drop.passphrase_required'),
            'passphrase',
        );
        await expectCreateFailure(
            coded(409, 'error.drop.cap_exhausted'),
            'cap',
        );
        await expectCreateFailure(
            coded(413, 'error.drop.file_too_large'),
            'too_large',
        );
        await expectCreateFailure(
            coded(400, 'error.upload.unsupported_content_type'),
            'unsupported_type',
        );
        await expectCreateFailure(
            coded(400, 'error.drop.malformed'),
            'generic',
        );
    });

    test('a refused passphrase is not read as an exhausted quota', async () => {
        // Both are 403. Switching on the status alone would turn "retype your passphrase" into
        // "this link's owner is out of space", which is unactionable and wrong.
        await expectCreateFailure(
            coded(403, 'error.drop.passphrase_required'),
            'passphrase',
        );
        await expectCreateFailure(coded(403, 'error.quota.exceeded'), 'quota');
    });

    test('a mid-upload chunk failure surfaces that chunk’s class', async () => {
        const sealed = fakeSealed(CHUNK_SIZE + 100);
        const progress: number[] = [];
        let patchCount = 0;
        let offset = 0;
        const { impl } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            patchCount += 1;
            if (patchCount === 2) return new Response(null, { status: 429 });
            offset += (call.body as Uint8Array).length;
            return chunkOk(offset);
        });

        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed,
            onProgress: (f) => progress.push(f),
            fetchImpl: impl,
        }).catch((e) => e);

        expect(err).toBeInstanceOf(DropUploadError);
        expect((err as DropUploadError).code).toBe('rate_limited');
        // The first chunk still reported progress before the second failed.
        expect(progress.length).toBe(1);
        expect(progress[0]).toBeCloseTo(CHUNK_SIZE / (CHUNK_SIZE + 100), 5);
    });
});

describe('uploadDrop — waiting out 503 and 507', () => {
    const NOW_MS = 1_800_000_000_000;

    const waitFor = (
        status: number,
        code: string,
        retryAfterHeader: string | null,
    ) =>
        new Response(
            JSON.stringify({
                code,
                retry_after: NOW_MS / 1000 + 300,
            }),
            {
                status,
                headers: {
                    'Content-Type': 'application/problem+json',
                    ...(retryAfterHeader === null
                        ? {}
                        : { 'Retry-After': retryAfterHeader }),
                },
            },
        );

    test('a full disk and an outage are waited out and the same chunk is resent at the same offset', async () => {
        const sealed = fakeSealed(CHUNK_SIZE + 100);
        const slept: number[] = [];
        const progress: number[] = [];
        let patchCount = 0;
        let offset = 0;
        const { impl, calls } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            patchCount += 1;
            if (patchCount === 2)
                return waitFor(507, 'error.drop.storage_full', '300');
            if (patchCount === 3)
                return waitFor(503, 'error.drop.chunk_unavailable', '30');
            offset += (call.body as Uint8Array).length;
            return chunkOk(offset);
        });

        await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed,
            onProgress: (f) => progress.push(f),
            fetchImpl: impl,
            sleep: async (ms) => {
                slept.push(ms);
            },
            now: () => NOW_MS,
        });

        expect(slept).toEqual([300_000, 30_000]);
        const patches = calls.filter((c) => c.method === 'PATCH');
        expect(patches.length).toBe(4);
        // The refused chunk is resent unchanged: same offset, same checksum, same bytes.
        for (const resent of patches.slice(1)) {
            expect(resent.headers.get('X-Capsule-Offset')).toBe(
                String(CHUNK_SIZE),
            );
            expect(resent.headers.get('X-Capsule-Checksum')).toBe(
                patches[1]?.headers.get('X-Capsule-Checksum') ?? null,
            );
            expect((resent.body as Uint8Array).length).toBe(100);
        }
        // A wait reports no progress of its own.
        expect(progress.length).toBe(2);
        expect(progress[1]).toBe(1);
    });

    test('a chunk that keeps being refused fails after the bounded number of waits', async () => {
        const slept: number[] = [];
        const { impl, calls } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            return waitFor(507, 'error.drop.storage_full', '300');
        });

        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
            sleep: async (ms) => {
                slept.push(ms);
            },
            now: () => NOW_MS,
        }).catch((e) => e);

        expect(err).toBeInstanceOf(DropUploadError);
        expect((err as DropUploadError).code).toBe('generic');
        expect(slept.length).toBe(MAX_CHUNK_WAITS);
        expect(calls.filter((c) => c.method === 'PATCH').length).toBe(
            MAX_CHUNK_WAITS + 1,
        );
    });

    test('a 500 is a failure, never a wait', async () => {
        const slept: number[] = [];
        const { impl } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            return new Response(
                JSON.stringify({ code: 'error.drop.unavailable' }),
                { status: 500 },
            );
        });

        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
            sleep: async (ms) => {
                slept.push(ms);
            },
        }).catch((e) => e);

        expect((err as DropUploadError).code).toBe('generic');
        expect(slept).toEqual([]);
    });
});

describe('uploadDrop — reading the deposit’s outcome', () => {
    const waiting = (newOffset: number) =>
        new Response(null, {
            status: 204,
            headers: {
                'X-Capsule-Offset': String(newOffset),
                'X-Capsule-Upload-Status': 'waiting_for_processing',
            },
        });

    const status = (body: Record<string, unknown>) =>
        new Response(JSON.stringify(body), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
        });

    const statusOf = (
        state: string,
        receivedBytes: number,
        errorCode?: string,
    ) =>
        status({
            status: state,
            received_bytes: receivedBytes,
            total_size: receivedBytes,
            ...(errorCode ? { error_code: errorCode } : {}),
        });

    test('a deposit still re-hashing is polled with backoff until it is filed', async () => {
        const slept: number[] = [];
        const answers = [
            statusOf('waiting_for_processing', 10),
            new Response(null, { status: 500 }),
            statusOf('waiting_for_processing', 10),
            statusOf('completed', 10),
        ];
        const { impl, calls } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            if (call.method === 'PATCH') return waiting(10);
            return answers.shift() ?? statusOf('completed', 10);
        });

        await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
            sleep: async (ms) => {
                slept.push(ms);
            },
        });

        const probes = calls.filter((c) => c.method === 'GET');
        expect(probes.length).toBe(4);
        expect(probes[0].url).toBe('/d/o/upload-123');
        expect(probes[0].headers.get('Accept')).toBe('application/json');
        // A probe the server could not answer is one more poll, not a failure.
        expect(slept).toEqual([1000, 2000, 4000, 8000]);
    });

    test('a refused deposit is the failure class of its code', async () => {
        const { impl } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            if (call.method === 'PATCH') return waiting(10);
            return statusOf(
                'failed_processing',
                10,
                'error.upload.content_hash_mismatch',
            );
        });

        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
            sleep: async () => {},
        }).catch((e) => e);

        expect(err).toBeInstanceOf(DropUploadError);
        expect((err as DropUploadError).code).toBe('generic');
    });

    test('a link that stops answering for the session is "unavailable"', async () => {
        const { impl } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            if (call.method === 'PATCH') return waiting(10);
            return new Response(null, { status: 404 });
        });

        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
            sleep: async () => {},
        }).catch((e) => e);

        expect((err as DropUploadError).code).toBe('unavailable');
    });

    test('a deposit with no outcome after the bounded polls fails', async () => {
        const slept: number[] = [];
        const { impl, calls } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            if (call.method === 'PATCH') return waiting(10);
            return statusOf('waiting_for_processing', 10);
        });

        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
            sleep: async (ms) => {
                slept.push(ms);
            },
        }).catch((e) => e);

        expect((err as DropUploadError).code).toBe('generic');
        expect(calls.filter((c) => c.method === 'GET').length).toBe(
            MAX_STATUS_POLLS,
        );
        expect(Math.max(...slept)).toBe(STATUS_POLL_MAX_MS);
    });

    test('a proxy timeout on the completing chunk reads the deposit instead of failing', async () => {
        const answers = [
            statusOf('waiting_for_processing', 10),
            statusOf('completed', 10),
        ];
        const { impl, calls } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            if (call.method === 'PATCH')
                return new Response(null, { status: 504 });
            return answers.shift() ?? statusOf('completed', 10);
        });
        const progress: number[] = [];

        await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            onProgress: (f) => progress.push(f),
            fetchImpl: impl,
            sleep: async () => {},
        });

        // One chunk, never resent: the session said it already holds every byte.
        expect(calls.filter((c) => c.method === 'PATCH').length).toBe(1);
        expect(calls.filter((c) => c.method === 'GET').length).toBe(2);
        expect(progress).toEqual([1]);
    });

    test('a proxy timeout mid-upload resumes from the session’s offset', async () => {
        const sealed = fakeSealed(2 * CHUNK_SIZE + 100);
        let patchCount = 0;
        let offset = 0;
        const { impl, calls } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            if (call.method === 'GET') return statusOf('uploading', offset);
            patchCount += 1;
            offset += (call.body as Uint8Array).length;
            // The first chunk lands, and its answer is lost behind a proxy.
            if (patchCount === 1) return new Response(null, { status: 502 });
            return chunkOk(offset);
        });

        await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed,
            fetchImpl: impl,
            sleep: async () => {},
        });

        const patches = calls.filter((c) => c.method === 'PATCH');
        expect(patches.map((p) => p.headers.get('X-Capsule-Offset'))).toEqual([
            '0',
            String(CHUNK_SIZE),
            String(2 * CHUNK_SIZE),
        ]);
    });

    test('a proxy timeout whose probe sees every byte but a pending session still awaits the deposit', async () => {
        const answers = [
            statusOf('uploading', 10),
            statusOf('failed_processing', 10, 'error.quota.exceeded'),
        ];
        const { impl, calls } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            if (call.method === 'PATCH')
                return new Response(null, { status: 504 });
            return answers.shift() ?? statusOf('completed', 10);
        });

        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
            sleep: async () => {},
        }).catch((e) => e);

        // Not resolved as filed: the deposit's refusal is read from the next probe.
        expect((err as DropUploadError).code).toBe('quota');
        expect(calls.filter((c) => c.method === 'PATCH').length).toBe(1);
        expect(calls.filter((c) => c.method === 'GET').length).toBe(2);
    });

    test('a proxy timeout over a deposit already refused names its class', async () => {
        const { impl } = mockFetch((call) => {
            if (call.method === 'POST') return created();
            if (call.method === 'PATCH')
                return new Response(null, { status: 504 });
            return statusOf('failed_processing', 10, 'error.quota.exceeded');
        });

        const err = await uploadDrop({
            base: '',
            opaqueId: 'o',
            sealed: fakeSealed(10),
            fetchImpl: impl,
            sleep: async () => {},
        }).catch((e) => e);

        expect((err as DropUploadError).code).toBe('quota');
    });

    test('the backoff doubles from a second to its ceiling', () => {
        expect(statusPollDelayMs(0)).toBe(1000);
        expect(statusPollDelayMs(3)).toBe(8000);
        expect(statusPollDelayMs(5)).toBe(STATUS_POLL_MAX_MS);
        expect(statusPollDelayMs(1000)).toBe(STATUS_POLL_MAX_MS);
    });
});

describe('retryDelayMs', () => {
    const NOW_MS = 1_800_000_000_000;

    test('reads Retry-After as delta-seconds', () => {
        expect(retryDelayMs('30', null, NOW_MS)).toBe(30_000);
    });

    test('reads Retry-After as an HTTP date', () => {
        const at = new Date(NOW_MS + 120_000).toUTCString();
        expect(retryDelayMs(at, null, NOW_MS)).toBe(120_000);
    });

    test('falls back to the body instant, then to the outage wait', () => {
        expect(retryDelayMs(null, NOW_MS / 1000 + 45, NOW_MS)).toBe(45_000);
        expect(retryDelayMs(null, null, NOW_MS)).toBe(30_000);
        expect(retryDelayMs('soon', null, NOW_MS)).toBe(30_000);
    });

    test('clamps to at least a second and at most the ceiling', () => {
        expect(retryDelayMs('0', null, NOW_MS)).toBe(1000);
        expect(retryDelayMs(null, NOW_MS / 1000 - 60, NOW_MS)).toBe(1000);
        expect(retryDelayMs('86400', null, NOW_MS)).toBe(MAX_WAIT_MS);
    });
});
