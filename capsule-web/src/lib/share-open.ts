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

/** The per-asset crypto-envelope params the serve response carries; exactly what
 *  {@link decryptShareBlob} feeds `ShareScope.decryptBlob`. */
export interface ShareAssetCryptoParams {
    /** The served `asset_id` (the file UUID the per-file key is derived for). */
    assetId: string;
    /** The crypto-manifest AMK epoch (`amk_version`); `0` for an asset-scoped grant. */
    amkVersion: number;
    /** Lowercase-hex of the asset's STREAM nonce prefix (`nonce_prefix`). */
    noncePrefixHex: string;
}

/**
 * Decrypt one covered asset's ciphertext blob to plaintext, entirely in the browser.
 *
 * The single decrypt path shared by the guest viewer and the cross-language KAT: it derives the
 * asset's per-file key from the opened {@link ShareScope} and STREAM-decrypts the `ciphertext`
 * (the bytes from `/s/{opaque-id}/blob/{hash}`), authenticated by the AEAD tag. Throws a stable
 * machine code (`scope_unavailable`, `tampered`, `malformed`) mapped by {@link shareOpenCode}.
 */
export function decryptShareBlob(
    scope: ShareScope,
    params: ShareAssetCryptoParams,
    ciphertext: Uint8Array,
): Uint8Array {
    return scope.decryptBlob(
        params.assetId,
        params.amkVersion,
        params.noncePrefixHex,
        ciphertext,
    );
}

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
