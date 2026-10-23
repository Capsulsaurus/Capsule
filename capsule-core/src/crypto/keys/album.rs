//! Album Master Keys (AMKs) and per-file/blob key derivation.
//!
//! Each album is an MLS group; its content key is a **random 32-byte AMK minted per
//! epoch** (`AMK_v{n}`) — never derived from MLS ratchet state. Per-file and per-blob
//! keys are derived from the AMK via HKDF-SHA512 with a scope-unique salt (the file/blob
//! UUID with the write's fresh nonce folded in, so a same-id rewrite re-rolls the key) and
//! a versioned label (SSoT: [Cryptography — Keys § Album Master Keys] and
//! [Encryption § Asset Key Derivation]).
//!
//! [Cryptography — Keys § Album Master Keys]: https://docs/design/cryptography/keys/#album-master-keys-amks
//! [Encryption § Asset Key Derivation]: https://docs/design/cryptography/encryption/#asset-key-derivation

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::primitives::info;
use crate::crypto::{kdf, rng};

/// The monotonic epoch identifier for an AMK (`amk_version`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AmkVersion(pub u32);

impl AmkVersion {
    /// The first epoch minted at album creation.
    pub const FIRST: AmkVersion = AmkVersion(1);

    /// The next epoch after this one.
    pub fn next(self) -> AmkVersion {
        AmkVersion(self.0 + 1)
    }
}

/// A random 32-byte album content key for one epoch. Holding it lets you decrypt; not
/// holding it means you cannot (secrecy is enforced by encryption, authorization by
/// signatures — see [`HybridSigningKey`](super::HybridSigningKey) write-tier keys).
#[derive(Clone)]
pub struct Amk([u8; 32]);

impl Amk {
    /// Mint a fresh random AMK for a new epoch.
    pub fn generate() -> Self {
        Self(rng::random_array::<32>())
    }

    /// Wrap raw AMK bytes (e.g. from the escrowed ledger).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw bytes (for escrow into the backup AMK ledger).
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derive the per-file AES-256 key:
    /// `HKDF(ikm=AMK, salt=file_id || nonce_prefix, info="asset-file/v1")`.
    ///
    /// The fresh `nonce_prefix` drawn for *this* encryption is folded into the salt, so a
    /// same-epoch [`replace`] (constant `file_id`) re-rolls the **key**, not merely the
    /// nonce — no `(file_key, nonce_prefix)` pair is ever reused, which is what lets the
    /// STREAM counter safely start at zero. The re-keying writers in
    /// `crypto::encryption`'s re-keying writers draw the nonce and derive together (the salt
    /// depends on the nonce). SSoT: [Encryption § Asset Key Derivation].
    ///
    /// [`replace`]: https://docs/design/cryptography/encryption/#re-keying-on-rewrite
    /// [Encryption § Asset Key Derivation]: https://docs/design/cryptography/encryption/#asset-key-derivation
    pub fn derive_file_key(&self, file_id: &Uuid, nonce_prefix: &[u8]) -> [u8; 32] {
        derive_scoped(&self.0, file_id, nonce_prefix, info::ASSET_FILE_V1)
    }

    /// Derive the per-metadata-blob AES-256 key:
    /// `HKDF(ikm=AMK, salt=blob_id || nonce, info="metadata-blob/v1")`.
    ///
    /// The blob's fresh 12-byte `nonce` is folded into the salt, so the key is re-derived
    /// per write even though `blob_id` (the asset id) is constant across a `metadata-update`.
    pub fn derive_blob_key(&self, blob_id: &Uuid, nonce: &[u8]) -> [u8; 32] {
        derive_scoped(&self.0, blob_id, nonce, info::METADATA_BLOB_V1)
    }

    /// Derive the AES-256 key that wraps an externally-chosen file key `K` for an adopted
    /// [web-upload drop](https://docs/design/web-upload/) (`key_mode = wrapped`):
    /// `HKDF(ikm=AMK, salt=file_id || wrap_nonce, info="asset-keywrap/v1")`.
    ///
    /// The fresh `wrap_nonce` is folded into the salt exactly as `nonce_prefix` is for a
    /// derived file key, so no `(wrap_key, wrap_nonce)` pair repeats even under a constant
    /// `file_id`. Used only by the keywrap seal/unseal path
    /// ([`crate::crypto::encryption::seal_file_key`]).
    pub fn derive_wrap_key(&self, file_id: &Uuid, wrap_nonce: &[u8]) -> [u8; 32] {
        derive_scoped(&self.0, file_id, wrap_nonce, info::ASSET_KEYWRAP_V1)
    }
}

/// Derive a 32-byte key salted on `id || fold`, the shared shape of every per-scope key
/// (file, blob, wrap): the scope UUID gives domain separation and the folded fresh nonce
/// makes the salt unique per write, so no `(key, nonce)` pair repeats.
fn derive_scoped(ikm: &[u8; 32], id: &Uuid, fold: &[u8], info: &[u8]) -> [u8; 32] {
    let mut salt = Vec::with_capacity(id.as_bytes().len() + fold.len());
    salt.extend_from_slice(id.as_bytes());
    salt.extend_from_slice(fold);
    kdf::derive_key32(ikm, &salt, info)
}

impl std::fmt::Debug for Amk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Amk(****)")
    }
}

#[cfg(test)]
mod tests;
