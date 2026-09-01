// Guest-drop uploader (slices S-D3, S-C61): chunking, checksums, progress, and failure
// classification, driven against a mocked fetch. No crypto here — the seal is exercised by the
// cross-language KAT; this pins the HTTP protocol `capsule-server`'s drop routes require, and it
// is what caught the paths and the body going stale when the Salvo tree retired.

import { describe, expect, test } from 'bun:test';

import type { SealedDropWire } from './drop-seal';
import { DropUploadError, sha256Hex, uploadDrop } from './drop-upload';

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
