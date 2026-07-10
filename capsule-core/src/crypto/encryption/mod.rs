//! Asset and metadata encryption — the only place AES-256-GCM is invoked for user data.
//!
//! Three constructions (SSoT: [Cryptography — Encryption]):
//! - [`stream`] — AES-256-GCM STREAM for asset bytes (originals + derivatives), supporting
//!   streaming, ranged reads, and per-chunk authentication.
//! - [`blob`] — standalone AES-256-GCM with a fixed wire format for small metadata blobs.
//! - [`keywrap`] — `asset-keywrap/v1` seal/unseal for an externally-chosen file key carried
//!   under `key_mode = wrapped` (an adopted web-upload drop).
//!
//! [Cryptography — Encryption]: https://docs/design/cryptography/encryption/

pub mod blob;
pub mod keywrap;
pub mod stream;

pub use blob::{blob_ciphertext_hash, open_blob, seal_blob};
pub use keywrap::{WRAP_NONCE_LEN, WRAPPED_FILE_KEY_LEN, seal_file_key, unseal_file_key};
pub use stream::{
    AssetEncryption, StreamError, decrypt_asset, decrypt_chunk, encrypt_asset,
    encrypt_asset_with_prefix,
};
