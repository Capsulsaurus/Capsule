//! Asset and metadata encryption — the only place AES-256-GCM is invoked for user data.
//!
//! Three constructions (SSoT: [Cryptography — Encryption]):
//! - `stream` — AES-256-GCM STREAM for asset bytes (originals + derivatives), supporting
//!   streaming, ranged reads, and per-chunk authentication.
//! - `blob` — standalone AES-256-GCM with a fixed wire format for small metadata blobs.
//! - `keywrap` — `asset-keywrap/v1` seal/unseal for an externally-chosen file key carried
//!   under `key_mode = wrapped` (an adopted web-upload drop).
//! - `rekey` — the re-keying writers that fold a fresh nonce into the key salt and refuse
//!   to reuse the nonce they supersede (a same-epoch `replace` / `metadata-update`).
//!
//! [Cryptography — Encryption]: https://docs/design/cryptography/encryption/

mod blob;
mod keywrap;
mod rekey;
mod stream;

// Every public item of the four modules, because this barrel is their only public path
// (#480). The chunk geometry and the `_vec` helpers were reachable only through the long
// path and are not incidental: `capsule-wasm` and `xtask`'s share KAT both re-derive an
// asset's nonce prefix, which is what makes a browser-opened share link decrypt to the same
// bytes the importer sealed.
pub use blob::{
    MIN_BLOB_LEN, NONCE_LEN, blob_ciphertext_hash, blob_nonce, blob_suite_id, open_blob, seal_blob,
    seal_blob_with_nonce,
};
pub use keywrap::{WRAP_NONCE_LEN, WRAPPED_FILE_KEY_LEN, seal_file_key, unseal_file_key};
pub use rekey::{
    encrypt_asset_rekey, encrypt_asset_rekey_with_prefix, seal_metadata_blob,
    seal_metadata_blob_with_nonce,
};
pub use stream::{
    AssetEncryption, CIPHERTEXT_CHUNK, NONCE_PREFIX_LEN, PLAINTEXT_CHUNK, StreamError, TAG_LEN,
    decrypt_asset, decrypt_asset_vec, decrypt_chunk, encrypt_asset, encrypt_asset_vec,
    encrypt_asset_vec_full, encrypt_asset_vec_with_prefix, encrypt_asset_with_prefix,
};
