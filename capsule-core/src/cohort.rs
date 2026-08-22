//! Device-cohort hash — the advisory session-grouping aid (SSoT:
//! [Authentication — Device Cohorts]; server storage + client emission are
//! slices `S-C13`/`S-D11` in the repo-root `SLICES.md`).
//!
//! One deterministic hash groups a physical device's sessions across app
//! reinstalls (reset-stability is platform-limited and documented honestly in
//! the owner doc). The construction is domain-separated canonical CBOR — never
//! naive concatenation — and folds the `user_id` in so the same device under
//! two accounts yields unlinkable hashes.
//!
//! **Advisory-only invariant:** the value is client-asserted and unverifiable;
//! no authorization or capability decision may ever read it.
//!
//! [Authentication — Device Cohorts]: https://docs/design/authentication/#device-cohorts

use ciborium::value::Value;
use uuid::Uuid;

use crate::cbor;
use crate::crypto::hash::{Hash32, hash_bytes};

/// Domain-separation label (versioned; a construction change bumps the suffix).
pub const DEVICE_COHORT_V1: &str = "capsule-device-cohort/v1";

/// Closed platform tag (wire value is the lowercase name; a new platform is an
/// additive protocol change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformTag {
    Ios,
    Android,
    Macos,
    Windows,
    Linux,
}

impl PlatformTag {
    pub fn as_str(self) -> &'static str {
        match self {
            PlatformTag::Ios => "ios",
            PlatformTag::Android => "android",
            PlatformTag::Macos => "macos",
            PlatformTag::Windows => "windows",
            PlatformTag::Linux => "linux",
        }
    }
}

/// The cohort hash:
/// `SHA-256( canonical-CBOR([ "capsule-device-cohort/v1", user_id, platform_tag, primary_id ]) )`.
///
/// `primary_id` is the platform's single primary identifier per the owner doc's
/// table (Keychain seed / SSAID / IOPlatformUUID / MachineGuid / machine-id) —
/// exactly one input per platform, chosen for stability, never a concatenated
/// fingerprint.
pub fn cohort_hash(user_id: Uuid, platform: PlatformTag, primary_id: &str) -> Hash32 {
    let array = Value::Array(vec![
        Value::Text(DEVICE_COHORT_V1.to_string()),
        Value::Bytes(user_id.as_bytes().to_vec()),
        Value::Text(platform.as_str().to_string()),
        Value::Text(primary_id.to_string()),
    ]);
    hash_bytes(&cbor::value_to_canonical_vec(&array))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "9774d56d682e549c";

    #[test]
    fn deterministic_across_invocations() {
        let u = Uuid::from_u128(0x1234);
        assert_eq!(
            cohort_hash(u, PlatformTag::Android, ID),
            cohort_hash(u, PlatformTag::Android, ID),
        );
    }

    #[test]
    fn user_id_fold_makes_accounts_unlinkable() {
        // Same physical device (same primary id), two accounts → distinct
        // hashes: the cross-account correlation surface is removed at the source.
        let a = cohort_hash(Uuid::from_u128(1), PlatformTag::Android, ID);
        let b = cohort_hash(Uuid::from_u128(2), PlatformTag::Android, ID);
        assert_ne!(a, b);
    }

    #[test]
    fn platform_and_label_are_domain_separating() {
        let u = Uuid::from_u128(0x1234);
        assert_ne!(
            cohort_hash(u, PlatformTag::Android, ID),
            cohort_hash(u, PlatformTag::Linux, ID),
        );
        // Structured encoding, not concatenation: shifting bytes between
        // components must change the hash.
        assert_ne!(
            cohort_hash(u, PlatformTag::Android, "ab"),
            cohort_hash(u, PlatformTag::Android, "a"),
        );
    }
}
