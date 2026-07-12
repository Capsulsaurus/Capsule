// Client-side guest-drop sealing surface (slice S-D3).
//
// Thin wrapper over the `capsule-wasm` module (built by `mise run build-wasm`). All the drop
// crypto — fresh per-asset key `K`, STREAM encryption, and KEM-encapsulation of `K` to the link's
// Drop Key — runs here in the browser: the URL `#{drop_pubkey}` fragment and any passphrase never
// reach the server (SSoT: the Web Upload design doc). The wasm side is byte-identical to the Rust
// core, proven by the cross-language KAT in `drop-seal.test.ts`.
//
// This surface is deliberately **contribute-only**: it can seal a drop, but there is no way to
// open/decrypt one — only the album owner's native client, holding the Drop Key private half, can
// adopt.

import init, {
    dropPassphraseProof,
    sealDrop,
    sealDropDerand,
    type WasmSealedDrop,
} from '@/generated/wasm/capsule_wasm';

export { dropPassphraseProof, sealDrop, sealDropDerand };

/**
 * A sealed drop, projected into exactly the wire shapes the drop endpoints expect. `size` is the
 * drop session's declared ciphertext length; `descriptor` is the unsigned `DropDescriptor` (hex
 * nonce prefix / ciphertext hash, base64 `kem_ct`); `ciphertext` is the STREAM ciphertext the
 * uploader streams in chunks.
 */
export interface SealedDropWire {
    size: number;
    descriptor: {
        content_type: string;
        plaintext_size: number;
        chunk_size: number;
        nonce_prefix: string;
        ciphertext_hash: string;
        kem_ct: string;
        suggested_filename: string | null;
    };
    ciphertext: Uint8Array;
}

/**
 * Stable machine error codes the wasm seal surface throws (as `Error.message`). Kept in sync with
 * `capsule-wasm/src/lib.rs::err`; the uploader/route map each to an i18n catalog key.
 */
export type DropSealCode = 'malformed' | 'seal_failed';

/** Extract the stable code from a thrown wasm seal error (its `Error.message`). */
export function dropSealCode(error: unknown): DropSealCode | 'unknown' {
    const message = error instanceof Error ? error.message : String(error);
    switch (message) {
        case 'malformed':
        case 'seal_failed':
            return message;
        default:
            return 'unknown';
    }
}

/** Project a live `WasmSealedDrop` into the plain wire object, freeing the wasm handle. */
function toWire(
    sealed: WasmSealedDrop,
    suggestedFilename: string | null,
): SealedDropWire {
    const wire: SealedDropWire = {
        size: sealed.ciphertextLen(),
        descriptor: {
            content_type: sealed.contentType(),
            plaintext_size: sealed.plaintextSize(),
            chunk_size: sealed.chunkSize(),
            nonce_prefix: sealed.noncePrefixHex(),
            ciphertext_hash: sealed.ciphertextHashHex(),
            kem_ct: sealed.kemCtB64(),
            suggested_filename: suggestedFilename,
        },
        // Copy the ciphertext out before freeing the wasm-owned handle.
        ciphertext: sealed.ciphertext(),
    };
    sealed.free();
    return wire;
}

/**
 * Seal one asset for a guest drop: STREAM-encrypt under a fresh `K` and encapsulate `K` to the
 * link's Drop Key public half. Throws (as `Error.message`) `seal_failed` on a malformed Drop Key.
 */
export function sealAsset(
    plaintext: Uint8Array,
    dropPubkey: Uint8Array,
    contentType: string,
    suggestedFilename: string | null,
): SealedDropWire {
    return toWire(
        sealDrop(plaintext, dropPubkey, contentType),
        suggestedFilename,
    );
}

let initialized: Promise<unknown> | null = null;

/**
 * Initialize the wasm module exactly once. In the browser call with no argument — the generated
 * glue fetches its sibling `.wasm`. Tests (and any non-browser host) pass the wasm bytes/module
 * explicitly. Shares the same singleton discipline as `share-open.ts::initShareWasm`.
 */
export function initDropWasm(
    input?: Parameters<typeof init>[0],
): Promise<unknown> {
    if (!initialized) {
        initialized = init(input);
    }
    return initialized;
}
