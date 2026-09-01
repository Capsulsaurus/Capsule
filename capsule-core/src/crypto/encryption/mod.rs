//! Asset and metadata encryption — the only place AES-256-GCM is invoked for user data.
//!
//! Three constructions (SSoT: [Cryptography — Encryption]):
//! - [`stream`] — AES-256-GCM STREAM for asset bytes (originals + derivatives), supporting
//!   streaming, ranged reads, and per-chunk authentication.
//! - [`blob`] — standalone AES-256-GCM with a fixed wire format for small metadata blobs.
//! - [`keywrap`] — `asset-keywrap/v1` seal/unseal for an externally-chosen file key carried
//!   under `key_mode = wrapped` (an adopted web-upload drop).
//! - [`rekey`] — the re-keying writers that fold a fresh nonce into the key salt and refuse
//!   to reuse the nonce they supersede (a same-epoch `replace` / `metadata-update`).
//!
//! [Cryptography — Encryption]: https://docs/design/cryptography/encryption/

pub mod blob;
pub mod keywrap;
pub mod rekey;
pub mod stream;

pub use blob::{blob_ciphertext_hash, blob_nonce, open_blob, seal_blob, seal_blob_with_nonce};
pub use keywrap::{WRAP_NONCE_LEN, WRAPPED_FILE_KEY_LEN, seal_file_key, unseal_file_key};
pub use rekey::{
    encrypt_asset_rekey, encrypt_asset_rekey_with_prefix, seal_metadata_blob,
    seal_metadata_blob_with_nonce,
};
pub use stream::{
    AssetEncryption, StreamError, decrypt_asset, decrypt_chunk, encrypt_asset,
    encrypt_asset_with_prefix,
};
