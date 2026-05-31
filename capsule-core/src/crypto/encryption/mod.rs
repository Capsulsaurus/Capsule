//! Asset and metadata encryption — the only place AES-256-GCM is invoked for user data.
//!
//! Two constructions (SSoT: [Cryptography — Encryption]):
//! - [`stream`] — AES-256-GCM STREAM for asset bytes (originals + derivatives), supporting
//!   streaming, ranged reads, and per-chunk authentication.
//! - [`blob`] — standalone AES-256-GCM with a fixed wire format for small metadata blobs.
//!
//! [Cryptography — Encryption]: https://docs/design/cryptography/encryption/

pub mod blob;
pub mod stream;

pub use blob::{blob_ciphertext_hash, open_blob, seal_blob};
pub use stream::{
    AssetEncryption, StreamError, decrypt_asset, decrypt_chunk, encrypt_asset,
    encrypt_asset_with_prefix,
};
