// Client-side share-link open surface (slice S-E1).
//
// Thin wrapper over the `capsule-wasm` module (built by `mise run build-wasm`). All the
// share-link crypto — Argon2id passphrase unwrap, link-secret decapsulation, and STREAM blob
// decryption — runs here in the browser: the URL `#{secret}` fragment and any passphrase never
// reach the server (SSoT: the Share Links design doc). The wasm side is byte-identical to the
// Rust issuer, proven by the cross-language KAT in `share-open.test.ts`.

import init, {
    openShare,
    openShareMetadata,
    type ShareScope,
    shareIsPassphraseProtected,
} from '@/generated/wasm/capsule_wasm';

export type { ShareScope };
export { openShare, openShareMetadata, shareIsPassphraseProtected };

/** One covered asset, as the sealed share metadata record describes it. */
export interface ShareAsset {
    assetId: string;
    /** The ciphertext's content address: the `{hash}` of `/s/{opaque-id}/blob/{hash}`. */
    contentHash: string;
    contentType: string;
    size: number;
    amkVersion: number;
    noncePrefixHex: string;
    keyMode: 'derived' | 'wrapped';
    wrappedFileKeyHex?: string;
}

/** The opened share metadata record: what the viewer lists and decrypts. */
export interface ShareMetadata {
    /** The issuer's RFC 3339 expiry, for display only. */
    expiresAt: string | null;
    assets: ShareAsset[];
}

/**
 * Where loading a share link lands, before any passphrase is asked for.
 *
 * - `unavailable`: the link is not served, or this URL's fragment does not open it. One state on
 *   purpose: a wrong fragment must look like a missing link.
 * - `redirect`: a peer that does not host the share pointed at its home server.
 * - `passphrase`: the record opened; the scope needs the passphrase to open.
 * - `open`: the record opened, and `wrapped` opens with the fragment alone.
 *
 * `wrapped` is the base64 canonical-CBOR `WrappedScope` that {@link openShare} takes.
 */
export type ShareLoad =
    | { kind: 'unavailable' }
    | { kind: 'redirect'; homeServer: string }
    | { kind: 'passphrase'; meta: ShareMetadata; wrapped: string }
    | { kind: 'open'; meta: ShareMetadata; wrapped: string };

/** The `GET /s/{opaque_id}` body (`SharedMetadataResponse`). */
interface SharedMetadataResponse {
    metadata_hash: string;
    passphrase_protected: boolean;
}

/** Base64 of raw octets, the encoding {@link openShare} takes its wrapped scope in. */
function toBase64(bytes: Uint8Array): string {
    let binary = '';
    for (const byte of bytes) binary += String.fromCharCode(byte);
    return btoa(binary);
}

/** Copy the wasm record into plain objects, so nothing the caller holds needs freeing. */
function readMetadata(
    view: ReturnType<typeof openShareMetadata>,
): ShareMetadata & { linkScope: string | undefined } {
    const assets = view.assets().map((asset) => {
        const record: ShareAsset = {
            assetId: asset.assetId,
            contentHash: asset.contentHash,
            contentType: asset.contentType,
            size: asset.size,
            amkVersion: asset.amkVersion,
            noncePrefixHex: asset.noncePrefixHex,
            keyMode: asset.keyMode === 'wrapped' ? 'wrapped' : 'derived',
            wrappedFileKeyHex: asset.wrappedFileKeyHex,
        };
        asset.free();
        return record;
    });
    const meta = {
        expiresAt: view.expiresAt() ?? null,
        assets,
        linkScope: view.linkScopeB64(),
    };
    view.free();
    return meta;
}

/**
 * Load a share link the way the server serves it: the metadata record (`GET /s/{opaque_id}`),
 * then the sealed record at its `metadata_hash` (`/s/{opaque_id}/blob/{hash}`), opened here with
 * the fragment, then — for a passphrase link only — the wrapped secret as octets
 * (`/s/{opaque_id}/wrapped-secret`). A link-only link's scope is inside the record.
 *
 * `fetchFn` is the transport, injected so the sequence is testable without a server. The wasm
 * must be initialized. Throws a stable code (`malformed`) for a record this reader cannot use,
 * and lets a network failure propagate; every refusal the server makes is `unavailable`.
 */
export async function loadShare(
    fetchFn: typeof fetch,
    base: string,
    opaqueId: string,
    fragment: string,
): Promise<ShareLoad> {
    const metaRes = await fetchFn(`${base}/s/${opaqueId}`);
    // A peer that does not host the share returns a home-server pointer (never content, never a
    // redirect), which the caller resolves.
    if (metaRes.status === 421) {
        const { home_server } = (await metaRes.json()) as {
            home_server: string;
        };
        return { kind: 'redirect', homeServer: home_server };
    }
    if (!metaRes.ok) return { kind: 'unavailable' };
    const served = (await metaRes.json()) as SharedMetadataResponse;

    const blobRes = await fetchFn(
        `${base}/s/${opaqueId}/blob/${served.metadata_hash}`,
    );
    if (!blobRes.ok) return { kind: 'unavailable' };
    const blob = new Uint8Array(await blobRes.arrayBuffer());

    let opened: ReturnType<typeof readMetadata>;
    try {
        opened = readMetadata(openShareMetadata(blob, opaqueId, fragment));
    } catch (error) {
        if (shareOpenCode(error) === 'wrong_secret')
            return { kind: 'unavailable' };
        throw error;
    }
    const { linkScope, ...meta } = opened;

    if (!served.passphrase_protected) {
        // A link with no passphrase has no wrapped secret on the server; its scope is in the
        // record, and a record without one cannot be opened.
        if (linkScope === undefined) throw new Error('malformed');
        return { kind: 'open', meta, wrapped: linkScope };
    }
    const wrappedRes = await fetchFn(`${base}/s/${opaqueId}/wrapped-secret`);
    if (!wrappedRes.ok) return { kind: 'unavailable' };
    const wrapped = toBase64(new Uint8Array(await wrappedRes.arrayBuffer()));
    return { kind: 'passphrase', meta, wrapped };
}

/** The per-asset crypto-envelope params the serve response carries; exactly what
 *  {@link decryptShareBlob} feeds `ShareScope.decryptBlob`. */
export interface ShareAssetCryptoParams {
    /** The served `asset_id` (the file UUID the per-file key is derived for). */
    assetId: string;
    /** The crypto-manifest AMK epoch (`amk_version`); `0` for an asset-scoped grant. */
    amkVersion: number;
    /** Lowercase-hex of the asset's STREAM nonce prefix (`nonce_prefix`). */
    noncePrefixHex: string;
    /** How the asset's file key is obtained (`key_mode`). Absent means `derived`, as it is
     *  wire-absent on a derived manifest; `wrapped` is an adopted web-upload drop. */
    keyMode?: 'derived' | 'wrapped';
    /** Lowercase-hex of the AMK-sealed `wrapped_file_key`; present iff `keyMode` is
     *  `wrapped`. */
    wrappedFileKeyHex?: string;
}

/**
 * Decrypt one covered asset's ciphertext blob to plaintext, entirely in the browser.
 *
 * The single decrypt path shared by the guest viewer and the cross-language KAT: it derives the
 * asset's per-file key from the opened {@link ShareScope} and STREAM-decrypts the `ciphertext`
 * (the bytes from `/s/{opaque-id}/blob/{hash}`), authenticated by the AEAD tag. A `wrapped`
 * asset's key is unwrapped from its `wrappedFileKeyHex` rather than re-derived. Throws a stable
 * machine code (`scope_unavailable`, `tampered`, `malformed`) mapped by {@link shareOpenCode};
 * `malformed` too when `keyMode` and `wrappedFileKeyHex` disagree, since a manifest carries the
 * wrapped key iff its mode is `wrapped`.
 */
export function decryptShareBlob(
    scope: ShareScope,
    params: ShareAssetCryptoParams,
    ciphertext: Uint8Array,
): Uint8Array {
    const wrapped = params.keyMode === 'wrapped';
    if (wrapped !== (params.wrappedFileKeyHex !== undefined)) {
        throw new Error('malformed');
    }
    return scope.decryptBlob(
        params.assetId,
        params.amkVersion,
        params.noncePrefixHex,
        ciphertext,
        wrapped ? params.wrappedFileKeyHex : undefined,
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
