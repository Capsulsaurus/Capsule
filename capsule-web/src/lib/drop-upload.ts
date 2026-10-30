// Guest-drop chunked uploader.
//
// Speaks the drop endpoints' HTTP protocol directly. It opens a drop session
// (`POST /d/{opaque-id}`) and streams the sealed ciphertext in chunks
// (`PATCH /d/{opaque-id}/{upload-id}`) using the upload protocol's chunk rules verbatim:
// `application/octet-stream`, an `X-Capsule-Offset`, and a per-chunk `X-Capsule-Checksum`
// (SHA-256 hex of the chunk body). It is deliberately minimal — sequential, fixed-size,
// 4 KiB-aligned chunks with no resumable/adaptive sophistication beyond what a guest upload
// needs. Contribute-only: the one thing it reads back is its own session's status
// (`GET /d/{opaque-id}/{upload-id}`), because the completing chunk is answered
// `waiting_for_processing` before the server's deposit has filed or refused the drop.
//
// It is hand-written rather than generated, and that is the one exception the SDK contract
// allows: a guest holds no account, so nothing here can ride `capsule-sdk`'s authenticated
// client, and the chunk loop is orchestration over three calls rather than a second parser.

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

/**
 * The chunk answers that leave whether the chunk landed unknown: a gateway (`502`) or a proxy
 * timeout (`504`) answered in the server's place. The session's status says where it stands, so
 * the uploader asks it rather than failing a drop the server may already be filing.
 */
const GATEWAY_STATUSES = new Set([502, 504]);

/** The first wait between status polls; each later one doubles, up to {@link STATUS_POLL_MAX_MS}. */
export const STATUS_POLL_INITIAL_MS = 1000;

/** The longest wait between two status polls. */
export const STATUS_POLL_MAX_MS = 30_000;

/**
 * How many status polls a deposit is given before the uploader stops asking: about eighteen
 * minutes in all, which a re-hash of the largest drop a browser seals comfortably fits.
 */
export const MAX_STATUS_POLLS = 40;

/** The wait before status poll `attempt` (zero-based): exponential, capped. */
export function statusPollDelayMs(attempt: number): number {
    return Math.min(
        STATUS_POLL_INITIAL_MS * 2 ** Math.min(attempt, 30),
        STATUS_POLL_MAX_MS,
    );
}

/** The parts of the drop status probe's answer this client reads. */
interface DropStatus {
    status: string;
    receivedBytes: number;
    errorCode: string | null;
}

/** What one status probe said: the session's state, a link that is gone, or nothing usable. */
type Probe =
    | { kind: 'status'; status: DropStatus }
    | { kind: 'gone' }
    | { kind: 'unknown' };

/** Ask the session's status once. Never throws: a network failure is `unknown`. */
async function probeStatus(
    fetchImpl: typeof fetch,
    url: string,
): Promise<Probe> {
    try {
        const res = await fetchImpl(url, {
            method: 'GET',
            headers: { Accept: 'application/json' },
        });
        if (res.status === 404) return { kind: 'gone' };
        if (!res.ok) return { kind: 'unknown' };
        const body = (await res.json()) as {
            status?: unknown;
            received_bytes?: unknown;
            error_code?: unknown;
        };
        if (typeof body.status !== 'string') return { kind: 'unknown' };
        return {
            kind: 'status',
            status: {
                status: body.status,
                receivedBytes:
                    typeof body.received_bytes === 'number'
                        ? body.received_bytes
                        : 0,
                errorCode:
                    typeof body.error_code === 'string'
                        ? body.error_code
                        : null,
            },
        };
    } catch {
        return { kind: 'unknown' };
    }
}

/** Whether the session's deposit has started, so no chunk is left to send. */
function depositStarted(status: string): boolean {
    return (
        status === 'waiting_for_processing' ||
        status === 'completed' ||
        status === 'failed_processing'
    );
}

/**
 * Poll the session's status with backoff until its deposit ends. Resolves once the drop is
 * filed (`completed`); rejects with the deposit's failure class once it is refused
 * (`failed_processing`, classified by its `error.*` code), with `unavailable` when the link stops
 * answering for the session, and with `generic` once {@link MAX_STATUS_POLLS} polls pass without
 * an outcome.
 */
async function awaitDeposit(
    fetchImpl: typeof fetch,
    url: string,
    sleep: (ms: number) => Promise<void>,
): Promise<void> {
    for (let attempt = 0; attempt < MAX_STATUS_POLLS; attempt++) {
        await sleep(statusPollDelayMs(attempt));
        const probe = await probeStatus(fetchImpl, url);
        if (probe.kind === 'gone') throw new DropUploadError('unavailable');
        if (probe.kind === 'unknown') continue;
        if (probe.status.status === 'completed') return;
        if (probe.status.status === 'failed_processing') {
            throw new DropUploadError(
                classifyFailure(0, probe.status.errorCode),
            );
        }
    }
    throw new DropUploadError('generic');
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
    /** Waits out a `503`/`507` before a chunk is resent (defaults to a timer); injectable. */
    sleep?: (ms: number) => Promise<void>;
    /** The wall clock in milliseconds, for an HTTP-date `Retry-After`; injectable for tests. */
    now?: () => number;
}

/**
 * Open a drop session and upload the sealed ciphertext in aligned chunks. Resolves when the drop
 * has been filed into the owner's inbox; rejects with a {@link DropUploadError} carrying the
 * failure class on any rejection (a mid-upload failure surfaces the class of the chunk that
 * failed). A chunk refused with `503` or `507` is a wait, not a failure: the uploader waits out
 * `Retry-After` and resends the same chunk at the same offset, up to {@link MAX_CHUNK_WAITS}
 * times, and only then fails with that chunk's class. A chunk answered `502` or `504` may have
 * landed: the uploader waits, asks the session's status, and continues from what it says.
 *
 * The completing chunk is answered `waiting_for_processing` while the server re-hashes and files
 * the drop, so the uploader then polls the session's status with backoff and reports the
 * deposit's outcome: filed, or refused with the deposit's failure class.
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
    const sessionUrl = `${base}/d/${opaqueId}/${uploadId}`;
    const ciphertext = sealed.ciphertext;
    const total = ciphertext.length;
    let offset = 0;
    let waits = 0;
    // The session's state after the last chunk. `null` when the server named none, which only a
    // server predating the status probe does: its completing chunk answered once the drop was
    // filed, so there is nothing to poll.
    let uploadStatus: string | null = null;
    while (offset < total) {
        const end = Math.min(offset + CHUNK_SIZE, total);
        const chunk = ciphertext.subarray(offset, end);
        const checksum = await sha256Hex(chunk);

        const chunkRes = await fetchImpl(sessionUrl, {
            method: 'PATCH',
            headers: {
                'Content-Type': 'application/octet-stream',
                'X-Capsule-Offset': String(offset),
                'X-Capsule-Checksum': checksum,
            },
            body: chunk as BodyInit,
        });
        if (!chunkRes.ok) {
            if (
                GATEWAY_STATUSES.has(chunkRes.status) &&
                waits < MAX_CHUNK_WAITS
            ) {
                // The chunk may have landed, and may have completed the drop: wait, then ask the
                // session rather than spend a guest's file on a drop the server is filing.
                waits += 1;
                await sleep(
                    retryDelayMs(
                        chunkRes.headers.get('Retry-After'),
                        null,
                        now(),
                    ),
                );
                const probe = await probeStatus(fetchImpl, sessionUrl);
                if (probe.kind === 'gone') {
                    throw new DropUploadError('unavailable');
                }
                if (probe.kind === 'status') {
                    const { status, errorCode } = probe.status;
                    if (status === 'failed_processing') {
                        throw new DropUploadError(
                            classifyFailure(0, errorCode),
                        );
                    }
                    if (depositStarted(status)) {
                        onProgress?.(1);
                        if (status === 'completed') return;
                        uploadStatus = status;
                        break;
                    }
                    const resumed = probe.status.receivedBytes;
                    if (Number.isFinite(resumed) && resumed > offset) {
                        offset = Math.min(resumed, total);
                        // The session may hold every byte yet still be `uploading`: the deposit
                        // outcome is then read after the loop, never assumed filed.
                        uploadStatus = status;
                        onProgress?.(
                            total === 0 ? 1 : Math.min(offset / total, 1),
                        );
                    }
                }
                // Otherwise resend from where the session stands; a chunk that did land is
                // answered as the replay it is.
                continue;
            }
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
        uploadStatus = chunkRes.headers.get('X-Capsule-Upload-Status');
        onProgress?.(total === 0 ? 1 : Math.min(offset / total, 1));
    }

    // 3. Every byte is with the server. Its deposit re-hashes and files the drop after the
    //    completing chunk was answered, so the outcome is read from the session's status.
    if (uploadStatus === null || uploadStatus === 'completed') return;
    await awaitDeposit(fetchImpl, sessionUrl, sleep);
}
