//! Device-cohort hash — the advisory session-grouping aid (SSoT:
//! [Authentication — Device Cohorts]).
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
mod tests;
