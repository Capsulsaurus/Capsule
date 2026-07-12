// Client-side share-link open surface (slice S-E1).
//
// Thin wrapper over the `capsule-wasm` module (built by `mise run build-wasm`). All the
// share-link crypto — Argon2id passphrase unwrap, link-secret decapsulation, and STREAM blob
// decryption — runs here in the browser: the URL `#{secret}` fragment and any passphrase never
// reach the server (SSoT: the Share Links design doc). The wasm side is byte-identical to the
// Rust issuer, proven by the cross-language KAT in `share-open.test.ts`.

import init, {
    openShare,
    type ShareScope,
    shareIsPassphraseProtected,
} from '@/generated/wasm/capsule_wasm';

export type { ShareScope };
export { openShare, shareIsPassphraseProtected };

/**
 * Stable machine error codes the wasm surface throws (as `Error.message`). Kept in sync with
 * `capsule-wasm/src/lib.rs::err`; the viewer maps each to an i18n catalog key.
 */
export type ShareOpenCode =
    | 'malformed'
    | 'passphrase_required'
    | 'wrong_secret'
    | 'scope_unavailable'
    | 'tampered';

/** Extract the stable code from a thrown wasm error (its `Error.message`). */
export function shareOpenCode(error: unknown): ShareOpenCode | 'unknown' {
    const message = error instanceof Error ? error.message : String(error);
    switch (message) {
        case 'malformed':
        case 'passphrase_required':
        case 'wrong_secret':
        case 'scope_unavailable':
        case 'tampered':
            return message;
        default:
            return 'unknown';
    }
}

let initialized: Promise<unknown> | null = null;

/**
 * Initialize the wasm module exactly once. In the browser call with no argument — the generated
 * glue fetches its sibling `.wasm`. Tests (and any non-browser host) pass the wasm bytes/module
 * explicitly.
 */
export function initShareWasm(
    input?: Parameters<typeof init>[0],
): Promise<unknown> {
    if (!initialized) {
        initialized = init(input);
    }
    return initialized;
}
