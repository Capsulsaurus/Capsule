// Guest-drop chunked uploader (slice S-D3).
//
// Speaks the drop endpoints' HTTP protocol directly — there is no generated client for the bare
// `#[handler]` drop routes. It opens a drop session (`POST /u/{opaque-id}/drop`) and streams the
// sealed ciphertext in chunks (`PATCH /u/{opaque-id}/drop/{id}`) using the upload protocol's chunk
// rules verbatim: `application/octet-stream`, an `X-Capsule-Offset`, and a per-chunk
// `X-Capsule-Checksum` (SHA-256 hex of the chunk body). It is deliberately minimal — sequential,
// fixed-size, 4 KiB-aligned chunks with no resumable/adaptive sophistication beyond what a guest
// upload needs. Contribute-only: it never lists or reads anything back.

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
    if (status === 404) return 'unavailable';
    if (status === 429) return 'rate_limited';
    if (status === 413) return 'too_large';
    switch (code) {
        case 'error.drop.cap_exceeded':
            return 'cap';
        case 'error.drop.passphrase_required':
            return 'passphrase';
        case 'error.drop.rate_limited':
            return 'rate_limited';
        case 'error.quota.exceeded':
        case 'error.quota.grace_locked':
            return 'quota';
        case 'error.upload.file_too_large':
            return 'too_large';
        case 'error.upload.unsupported_content_type':
            return 'unsupported_type';
        default:
            return 'generic';
    }
}

/** Read a rejection response's stable `error.*` code, if it carries a JSON `{ code }` body. */
async function readErrorCode(res: Response): Promise<string | null> {
    try {
        const text = await res.text();
        if (!text) return null;
        const body = JSON.parse(text) as { code?: unknown };
        return typeof body.code === 'string' ? body.code : null;
    } catch {
        return null;
    }
}

/** Turn any non-ok response into a typed `DropUploadError`. */
async function toUploadError(res: Response): Promise<DropUploadError> {
    return new DropUploadError(
        classifyFailure(res.status, await readErrorCode(res)),
    );
}

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
}

/**
 * Open a drop session and upload the sealed ciphertext in aligned chunks. Resolves when the drop
 * has been finalized into the owner's inbox; rejects with a {@link DropUploadError} carrying the
 * failure class on any rejection (a mid-upload failure surfaces the class of the chunk that
 * failed).
 */
export async function uploadDrop(opts: UploadDropOptions): Promise<void> {
    const {
        base,
        opaqueId,
        sealed,
        passphraseProof,
        onProgress,
        fetchImpl = fetch,
    } = opts;

    // 1. Open the drop session. The descriptor + declared size are validated server-side here.
    const createRes = await fetchImpl(`${base}/u/${opaqueId}/drop`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
            size: sealed.size,
            passphrase_proof: passphraseProof ?? null,
            descriptor: sealed.descriptor,
        }),
    });
    if (!createRes.ok) throw await toUploadError(createRes);
    const { drop_id: dropId } = (await createRes.json()) as { drop_id: string };

    // 2. Stream the ciphertext in sequential, aligned chunks until the server has all of it.
    const ciphertext = sealed.ciphertext;
    const total = ciphertext.length;
    let offset = 0;
    while (offset < total) {
        const end = Math.min(offset + CHUNK_SIZE, total);
        const chunk = ciphertext.subarray(offset, end);
        const checksum = await sha256Hex(chunk);

        const chunkRes = await fetchImpl(
            `${base}/u/${opaqueId}/drop/${dropId}`,
            {
                method: 'PATCH',
                headers: {
                    'Content-Type': 'application/octet-stream',
                    'X-Capsule-Offset': String(offset),
                    'X-Capsule-Checksum': checksum,
                },
                body: chunk as BodyInit,
            },
        );
        if (!chunkRes.ok) throw await toUploadError(chunkRes);

        // The server echoes the authoritative new offset; fall back to our own bookkeeping.
        const nextHeader = chunkRes.headers.get('X-Capsule-Offset');
        const next = nextHeader === null ? end : Number(nextHeader);
        offset = Number.isFinite(next) && next > offset ? next : end;
        onProgress?.(total === 0 ? 1 : Math.min(offset / total, 1));
    }
}
