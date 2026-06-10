//! The single source of truth, in code, for Capsule's cryptographic primitive set.
//!
//! Mirrors the inventory in [Cryptography — Primitives]. Every on-disk and on-wire
//! structure that depends on a primitive carries a [`SuiteId`] (`crypto_suite_id`), so two
//! structures encrypted under different suite versions can coexist without a flag day.
//! Retiring a primitive does not edit a row — it adds a new [`SuiteId`] variant.
//!
//! [Cryptography — Primitives]: https://docs/design/cryptography/primitives/

/// `crypto_suite_id` of the current primitive bundle (the [`SuiteId::V1`] inventory).
pub const CRYPTO_SUITE_ID: u16 = 0x0001;

/// The date-based wire `protocol_version` this build writes. Pinned per album at creation.
pub const PROTOCOL_VERSION: &str = "2026-05-31";

/// Identifies a complete bundle of cryptographic primitives.
///
/// A structure declaring a `crypto_suite_id` outside this closed set is rejected
/// (fail-closed), never best-effort-parsed under a guessed suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SuiteId {
    /// `0x0001`: SHA-256 · HKDF-SHA512 · Argon2id · AES-256-GCM(+STREAM) ·
    /// Ed25519+ML-DSA-65 · X-Wing · MLS `0x004D`.
    V1,
}

impl SuiteId {
    /// The suite new writes use.
    pub const CURRENT: SuiteId = SuiteId::V1;

    /// Map a wire `crypto_suite_id` to its suite, or `None` if this build does not
    /// implement it (the caller then fails closed — see invariant 2).
    pub fn from_u16(id: u16) -> Option<Self> {
        match id {
            CRYPTO_SUITE_ID => Some(SuiteId::V1),
            _ => None,
        }
    }

    /// The wire `crypto_suite_id` for this suite.
    pub fn as_u16(self) -> u16 {
        match self {
            SuiteId::V1 => CRYPTO_SUITE_ID,
        }
    }

    /// SHA-256 digest length under this suite (content-hash / `hash` field length).
    pub fn hash_len(self) -> usize {
        match self {
            SuiteId::V1 => 32,
        }
    }
}

/// Versioned HKDF `info` labels. Including a version string lets the KDF be rotated
/// later without a flag day; the SSoT for each label is the doc that derives that key.
pub mod info {
    /// Per-file asset key: `HKDF(ikm=AMK, salt=file_id, info=ASSET_FILE_V1)`.
    pub const ASSET_FILE_V1: &[u8] = b"asset-file/v1";
    /// Per-metadata-blob key: `HKDF(ikm=AMK, salt=blob_id, info=METADATA_BLOB_V1)`.
    pub const METADATA_BLOB_V1: &[u8] = b"metadata-blob/v1";
    /// Default-album *identifier* derived from the account master key (an ID, not a key).
    pub const DEFAULT_ALBUM_ID_V1: &[u8] = b"default-album-id/v1";
}

/// Device hardware tier, selecting Argon2id cost parameters at *wrap* time. The chosen
/// parameters are recorded in the wrapped blob, so unwrap works on any tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum DeviceTier {
    /// ≤ 2 GiB total RAM (entry-level Android / embedded).
    LowRam,
    /// Default for phones and laptops.
    Normal,
    /// ≥ 8 GiB; used when wrapping new escrow blobs from a desktop.
    Desktop,
}

/// Argon2id cost parameters: memory in KiB, iteration count `t`, parallelism `p`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    /// Memory cost in KiB.
    pub mem_kib: u32,
    /// Iteration (time) cost `t`.
    pub t_cost: u32,
    /// Degree of parallelism `p`.
    pub p_cost: u32,
}

impl DeviceTier {
    /// Canonical Argon2id parameters for this tier (see the primitives doc's table).
    pub fn params(self) -> Argon2Params {
        match self {
            DeviceTier::LowRam => Argon2Params {
                mem_kib: 128 * 1024,
                t_cost: 3,
                p_cost: 1,
            },
            DeviceTier::Normal => Argon2Params {
                mem_kib: 256 * 1024,
                t_cost: 3,
                p_cost: 1,
            },
            DeviceTier::Desktop => Argon2Params {
                mem_kib: 512 * 1024,
                t_cost: 4,
                p_cost: 1,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_id_round_trip_dispatches_to_exactly_one_row() {
        assert_eq!(SuiteId::from_u16(0x0001), Some(SuiteId::V1));
        assert_eq!(SuiteId::V1.as_u16(), 0x0001);
        assert_eq!(SuiteId::CURRENT, SuiteId::V1);
        assert_eq!(SuiteId::V1.hash_len(), 32);
    }

    #[test]
    fn unknown_suite_ids_fail_closed() {
        assert_eq!(SuiteId::from_u16(0x0000), None);
        assert_eq!(SuiteId::from_u16(0x0002), None);
        assert_eq!(SuiteId::from_u16(0xffff), None);
    }

    #[test]
    fn device_tier_params_match_inventory() {
        assert_eq!(
            DeviceTier::LowRam.params(),
            Argon2Params {
                mem_kib: 131_072,
                t_cost: 3,
                p_cost: 1
            }
        );
        assert_eq!(
            DeviceTier::Normal.params(),
            Argon2Params {
                mem_kib: 262_144,
                t_cost: 3,
                p_cost: 1
            }
        );
        assert_eq!(
            DeviceTier::Desktop.params(),
            Argon2Params {
                mem_kib: 524_288,
                t_cost: 4,
                p_cost: 1
            }
        );
    }

    #[test]
    fn info_labels_are_versioned_and_distinct() {
        // Distinct labels keep derived keys in separate domains.
        assert_ne!(info::ASSET_FILE_V1, info::METADATA_BLOB_V1);
        assert_ne!(info::ASSET_FILE_V1, info::DEFAULT_ALBUM_ID_V1);
        assert!(info::ASSET_FILE_V1.ends_with(b"/v1"));
        assert!(info::METADATA_BLOB_V1.ends_with(b"/v1"));
    }
}
