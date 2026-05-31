//! Album Master Keys (AMKs) and per-file/blob key derivation.
//!
//! Each album is an MLS group; its content key is a **random 32-byte AMK minted per
//! epoch** (`AMK_v{n}`) — never derived from MLS ratchet state. Per-file and per-blob
//! keys are derived from the AMK via HKDF-SHA512 with a scope-unique salt (the file/blob
//! UUID) and a versioned label (SSoT: [Cryptography — Keys § Album Master Keys] and
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
/// signatures — see [`super::hybrid_sig`] write-tier keys).
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

    /// Derive the per-file AES-256 key: `HKDF(ikm=AMK, salt=file_id, info="asset-file/v1")`.
    /// A fresh derived key per file lets the STREAM nonce counter safely start at zero.
    pub fn derive_file_key(&self, file_id: &Uuid) -> [u8; 32] {
        kdf::derive_key32(&self.0, file_id.as_bytes(), info::ASSET_FILE_V1)
    }

    /// Derive the per-metadata-blob AES-256 key:
    /// `HKDF(ikm=AMK, salt=blob_id, info="metadata-blob/v1")`.
    pub fn derive_blob_key(&self, blob_id: &Uuid) -> [u8; 32] {
        kdf::derive_key32(&self.0, blob_id.as_bytes(), info::METADATA_BLOB_V1)
    }
}

impl std::fmt::Debug for Amk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Amk(****)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_advance_monotonically() {
        assert_eq!(AmkVersion::FIRST, AmkVersion(1));
        assert_eq!(AmkVersion(3).next(), AmkVersion(4));
        assert!(AmkVersion(2) < AmkVersion(3));
    }

    #[test]
    fn file_key_is_deterministic_per_file() {
        let amk = Amk::from_bytes([7u8; 32]);
        let f = Uuid::from_u128(0x1234);
        assert_eq!(amk.derive_file_key(&f), amk.derive_file_key(&f));
    }

    #[test]
    fn distinct_files_blobs_and_amks_yield_distinct_keys() {
        let amk = Amk::from_bytes([7u8; 32]);
        let f1 = Uuid::from_u128(1);
        let f2 = Uuid::from_u128(2);
        // Different file_id → different key.
        assert_ne!(amk.derive_file_key(&f1), amk.derive_file_key(&f2));
        // File vs blob domain separation for the *same* id.
        assert_ne!(amk.derive_file_key(&f1), amk.derive_blob_key(&f1));
        // Different AMK epoch → different key.
        assert_ne!(
            amk.derive_file_key(&f1),
            Amk::from_bytes([8u8; 32]).derive_file_key(&f1)
        );
    }
}
