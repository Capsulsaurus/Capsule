// Guest-drop chunked uploader.
//
// Speaks the drop endpoints' HTTP protocol directly. It opens a drop session
// (`POST /d/{opaque-id}`) and streams the sealed ciphertext in chunks
// (`PATCH /d/{opaque-id}/{upload-id}`) using the upload protocol's chunk rules verbatim:
// `application/octet-stream`, an `X-Capsule-Offset`, and a per-chunk `X-Capsule-Checksum`
// (SHA-256 hex of the chunk body). It is deliberately minimal — sequential, fixed-size,
// 4 KiB-aligned chunks with no resumable/adaptive sophistication beyond what a guest upload
// needs. Contribute-only: it never lists or reads anything back.
//
// It is hand-written rather than generated, and that is the one exception the SDK contract
// allows: a guest holds no account, so nothing here can ride `capsule-sdk`'s authenticated
// client, and the chunk loop is orchestration over two calls rather than a second parser.

import type { SealedDropWire } from './drop-seal';

/**
 * The client-facing failure classes a drop upload can surface. Each maps 1:1 to a `drop.error.*`
 * i18n catalog key in the route. A not-found / revoked / expired link is the deliberately
 * indistinguishable `unavailable` (a bare `404`, no body) — never distinguished from a probe.
 */
export type DropFailureCode =
    | 'unavailable'
    | 'rate_limited'
    | 'too_large'
    | 'quota'
    | 'cap'
    | 'passphrase'
    | 'unsupported_type'
    | 'generic';

/** A drop upload failure carrying the stable class the route localizes. */
export class DropUploadError extends Error {
    readonly code: DropFailureCode;
    constructor(code: DropFailureCode) {
        super(code);
        this.name = 'DropUploadError';
        this.code = code;
    }
}

/**
 * Chunk size for drop uploads: 1 MiB, a multiple of 4096 so every non-final chunk satisfies the
 * upload protocol's 4 KiB alignment rule (invariant 10). Comfortably under the server's 16 MiB
 * per-chunk ceiling.
 */
const CHUNK_SIZE = 1 << 20;

/** Lowercase-hex SHA-256 of `bytes` — the `X-Capsule-Checksum` wire format (matches the server's
 *  `hash_bytes`). Uses Web Crypto, available in browsers and the bun test runner alike. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
    const digest = await crypto.subtle.digest('SHA-256', bytes as BufferSource);
    return Array.from(new Uint8Array(digest))
        .map((b) => b.toString(16).padStart(2, '0'))
        .join('');
}

/** Map a rejection (HTTP status + optional stable `error.*` code) to a client failure class. */
function classifyFailure(status: number, code: string | null): DropFailureCode {
    // The code is checked first and the status is the fallback. Two of the codes below share a
    // status with each other — a refused passphrase and an exhausted quota are both `403` — so
    // switching on the status alone would collapse "retype your passphrase" into "this link's
    // owner is out of space".
    switch (code) {
        case 'error.drop.cap_exhausted':
            return 'cap';
        case 'error.drop.passphrase_required':
            return 'passphrase';
        case 'error.drop.rate_limited':
            return 'rate_limited';
        case 'error.quota.exceeded':
        case 'error.quota.grace_locked':
        case 'error.quota.suspended':
            return 'quota';
        case 'error.drop.file_too_large':
        case 'error.upload.file_too_large':
            return 'too_large';
        case 'error.upload.unsupported_content_type':
            return 'unsupported_type';
        default:
            break;
    }
    if (status === 404) return 'unavailable';
    if (status === 429) return 'rate_limited';
    if (status === 413) return 'too_large';
    return 'generic';
}

/** The parts of a problem body this client reads. */
interface ProblemBody {
    code: string | null;
    retryAfter: number | null;
}

/** Read a rejection's stable `error.*` code and `retry_after` instant, where it carries them. */
async function readProblem(res: Response): Promise<ProblemBody> {
    try {
        const text = await res.text();
        if (!text) return { code: null, retryAfter: null };
        const body = JSON.parse(text) as {
            code?: unknown;
            retry_after?: unknown;
        };
        return {
            code: typeof body.code === 'string' ? body.code : null,
            retryAfter:
                typeof body.retry_after === 'number' ? body.retry_after : null,
        };
    } catch {
        return { code: null, retryAfter: null };
    }
}

/** Turn any non-ok response into a typed `DropUploadError`. */
async function toUploadError(res: Response): Promise<DropUploadError> {
    return new DropUploadError(
        classifyFailure(res.status, (await readProblem(res)).code),
    );
}

/**
 * The chunk refusals that are waits rather than failures: `503` (a store could not answer) and
 * `507` (the server's disk is full). Nothing of the refused chunk was kept, so the same chunk at
 * the same offset lands once the wait is over — or is answered as the replay it is.
 */
const WAIT_STATUSES = new Set([503, 507]);

/**
 * How many times one chunk is resent after a wait before the upload gives up. Bounded so a
 * server whose disk never frees cannot hold a guest's tab open forever: at the server's own
 * five-minute full-disk wait this is twenty-five minutes.
 */
export const MAX_CHUNK_WAITS = 5;

/** The wait used when a refusal names none: the server's own outage wait. */
const DEFAULT_WAIT_MS = 30_000;

/** The longest single wait honoured, so a hostile or broken `Retry-After` cannot park a tab. */
export const MAX_WAIT_MS = 10 * 60_000;

/**
 * How long to wait before resending a refused chunk, in milliseconds.
 *
 * `Retry-After` first (delta-seconds, or an HTTP date), then the body's `retry_after` instant
 * (Unix seconds), then {@link DEFAULT_WAIT_MS}; clamped to `[1 s, MAX_WAIT_MS]`.
 */
export function retryDelayMs(
    header: string | null,
    retryAfter: number | null,
    nowMs: number,
): number {
    let ms: number | null = null;
    if (header !== null && header.trim() !== '') {
        const trimmed = header.trim();
        if (/^\d+$/.test(trimmed)) {
            ms = Number(trimmed) * 1000;
        } else {
            const at = Date.parse(trimmed);
            if (Number.isFinite(at)) ms = at - nowMs;
        }
    }
    if (ms === null && retryAfter !== null) ms = retryAfter * 1000 - nowMs;
    if (ms === null || !Number.isFinite(ms)) ms = DEFAULT_WAIT_MS;
    return Math.min(Math.max(ms, 1000), MAX_WAIT_MS);
}

const realSleep = (ms: number) =>
    new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Options for {@link uploadDrop}. `fetchImpl` is injectable for tests. */
export interface UploadDropOptions {
    /** Base origin for the drop endpoints (empty = same-origin). */
    base: string;
    /** The link's `{opaque-id}` URL path component. */
    opaqueId: string;
    /** The sealed drop (from `drop-seal`'s `sealAsset`). */
    sealed: SealedDropWire;
    /** The Argon2id proof for a passphrase-gated link (lowercase hex), else null/undefined. */
    passphraseProof?: string | null;
    /** Progress callback, `fraction` in `[0, 1]`, invoked after each acknowledged chunk. */
    onProgress?: (fraction: number) => void;
    /** Fetch implementation (defaults to the global `fetch`); injectable for tests. */
    fetchImpl?: typeof fetch;
    /** Waits out a `503`/`507` before a chunk is resent (defaults to a timer); injectable. */
    sleep?: (ms: number) => Promise<void>;
    /** The wall clock in milliseconds, for an HTTP-date `Retry-After`; injectable for tests. */
    now?: () => number;
}

/**
 * Open a drop session and upload the sealed ciphertext in aligned chunks. Resolves when the drop
 * has been finalized into the owner's inbox; rejects with a {@link DropUploadError} carrying the
 * failure class on any rejection (a mid-upload failure surfaces the class of the chunk that
 * failed). A chunk refused with `503` or `507` is a wait, not a failure: the uploader waits out
 * `Retry-After` and resends the same chunk at the same offset, up to {@link MAX_CHUNK_WAITS}
 * times, and only then fails with that chunk's class.
 */
export async function uploadDrop(opts: UploadDropOptions): Promise<void> {
    const {
        base,
        opaqueId,
        sealed,
        passphraseProof,
        onProgress,
        fetchImpl = fetch,
        sleep = realSleep,
        now = Date.now,
    } = opts;

    // 1. Open the drop session. The descriptor + declared size are validated server-side here.
    const createRes = await fetchImpl(`${base}/d/${opaqueId}`, {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            Accept: 'application/json',
        },
        body: JSON.stringify({
            content_type: sealed.descriptor.content_type,
            size: sealed.size,
            ciphertext_hash: sealed.descriptor.ciphertext_hash,
            kem_ct: sealed.descriptor.kem_ct,
            // Absent rather than null when there is no passphrase: the body is strict, and a
            // present-null is a value the server would have to decide what to do with.
            ...(passphraseProof ? { passphrase_proof: passphraseProof } : {}),
            ...(sealed.descriptor.suggested_filename
                ? { suggested_filename: sealed.descriptor.suggested_filename }
                : {}),
        }),
    });
    if (!createRes.ok) throw await toUploadError(createRes);
    const { upload_id: uploadId } = (await createRes.json()) as {
        upload_id: string;
    };

    // 2. Stream the ciphertext in sequential, aligned chunks until the server has all of it.
    const ciphertext = sealed.ciphertext;
    const total = ciphertext.length;
    let offset = 0;
    let waits = 0;
    while (offset < total) {
        const end = Math.min(offset + CHUNK_SIZE, total);
        const chunk = ciphertext.subarray(offset, end);
        const checksum = await sha256Hex(chunk);

        const chunkRes = await fetchImpl(`${base}/d/${opaqueId}/${uploadId}`, {
            method: 'PATCH',
            headers: {
                'Content-Type': 'application/octet-stream',
                'X-Capsule-Offset': String(offset),
                'X-Capsule-Checksum': checksum,
            },
            body: chunk as BodyInit,
        });
        if (!chunkRes.ok) {
            if (WAIT_STATUSES.has(chunkRes.status) && waits < MAX_CHUNK_WAITS) {
                // Nothing of this chunk was kept: wait, then resend it at the same offset.
                waits += 1;
                const problem = await readProblem(chunkRes);
                await sleep(
                    retryDelayMs(
                        chunkRes.headers.get('Retry-After'),
                        problem.retryAfter,
                        now(),
                    ),
                );
                continue;
            }
            throw await toUploadError(chunkRes);
        }
        waits = 0;

        // The server echoes the authoritative new offset; fall back to our own bookkeeping.
        const nextHeader = chunkRes.headers.get('X-Capsule-Offset');
        const next = nextHeader === null ? end : Number(nextHeader);
        offset = Number.isFinite(next) && next > offset ? next : end;
        onProgress?.(total === 0 ? 1 : Math.min(offset / total, 1));
    }
}
