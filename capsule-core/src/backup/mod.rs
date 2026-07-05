//! The portable backup artifact and recovery mechanisms (SSoT: [Backup and Recovery]).
//!
//! Two distinct things are kept separate:
//! - the **[backup artifact](artifact)** — a deterministic, self-describing, signed tar of a
//!   library's encrypted blobs, metadata blobs, provenance chains, and the AMK ledger needed
//!   to decrypt them;
//! - the **[master-key escrow](escrow_master_key)** — a small passphrase-wrapped blob that
//!   reconstructs the key hierarchy.
//!
//! Recovery rests on one rule: holding the recovery secret restores every asset, even after
//! every device is lost. The artifact carries its own AMK ledger, so it is self-sufficient.
//!
//! [Backup and Recovery]: https://docs/design/backup-recovery/

pub mod artifact;

pub use artifact::{
    BackupArtifact, BackupAsset, BackupInput, RestoreMode, RestoreReport, export, export_with_salt,
};
use thiserror::Error;

use crate::crypto::primitives::DeviceTier;
use crate::crypto::pwkdf::{self, WrappedSecret};
use crate::crypto::{CryptoError, kdf, rng};

/// The backup artifact format version.
pub const ARTIFACT_FORMAT_VERSION: u16 = 1;

/// Errors from backup export/restore and recovery.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BackupError {
    /// A reader/writer or archive structural failure.
    #[error("backup io/format error: {0}")]
    Format(String),
    /// MANIFEST HMAC or exporter signature failed (tamper).
    #[error("backup authentication failed: {0}")]
    Auth(&'static str),
    /// An entry's content hash did not match the manifest (corruption).
    #[error("backup entry corrupt: {0}")]
    Corrupt(String),
    /// The AMK ledger is missing an `amk_version` an included asset references.
    #[error("backup AMK ledger incomplete: missing {0}")]
    AmkIncomplete(String),
    /// Underlying cryptographic failure.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

// ── Master-key escrow ───────────────────────────────────────────────────────

/// Escrow the account master key under a recovery passphrase (Argon2id + AES-256-GCM).
pub fn escrow_master_key(
    master: &[u8; 32],
    passphrase: &[u8],
    tier: DeviceTier,
) -> Result<WrappedSecret, BackupError> {
    Ok(pwkdf::wrap(master, passphrase, tier)?)
}

/// Recover the master key from its escrow blob and the recovery passphrase.
pub fn recover_master_key(
    blob: &WrappedSecret,
    passphrase: &[u8],
) -> Result<[u8; 32], BackupError> {
    let bytes = pwkdf::unwrap(blob, passphrase)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| BackupError::Auth("escrowed master key wrong length"))
}

// ── Recovery verification cadence (S-D12) ───────────────────────────────────

/// Domain-separation label for the recovery-verification derived-tag compare
/// (SSoT: [Backup — Recovery Verification Cadence]).
///
/// [Backup — Recovery Verification Cadence]: https://docs/design/backup-recovery/#recovery-verification-cadence
pub const RECOVERY_VERIFY_V1: &[u8] = b"capsule-recovery-verify/v1";

/// Outcome of a local recovery-secret verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The entered secret unwraps the (cached) escrow to the device-held master key.
    Verified,
    /// The entered secret does not open the blob — wrong secret, or the cached
    /// blob is stale after a rotation. Per the stale-cache rule the caller
    /// refreshes the blob and retries once before recording a failure.
    NotVerified,
}

/// Local-only verification that the user still holds the recovery secret:
/// unwrap the cached escrow blob, then compare an HKDF-derived tag against the
/// tag derived from the device-held master key (derived-tag compare — raw key
/// bytes are never compared or surfaced). Zero network I/O by construction; the
/// cadence, snooze, and refresh-before-fail policy live in the client (S-D12).
pub fn verify_recovery_secret(
    cached_escrow: &WrappedSecret,
    passphrase: &[u8],
    device_master: &[u8; 32],
) -> VerifyOutcome {
    let Ok(unwrapped) = recover_master_key(cached_escrow, passphrase) else {
        return VerifyOutcome::NotVerified;
    };
    let tag_a = kdf::hkdf_sha512(&unwrapped, b"", RECOVERY_VERIFY_V1, 32);
    let tag_b = kdf::hkdf_sha512(device_master, b"", RECOVERY_VERIFY_V1, 32);
    if tag_a == tag_b {
        VerifyOutcome::Verified
    } else {
        VerifyOutcome::NotVerified
    }
}

// ── Opt-in Shamir 2-of-3 social recovery ────────────────────────────────────

/// Split a 32-byte recovery seed into 3 Shamir shares; any 2 reconstruct it, 1 reveals
/// nothing. The default scheme from [Backup — Opt-in: Shamir Secret Sharing].
///
/// [Backup — Opt-in: Shamir Secret Sharing]: https://docs/design/backup-recovery/#opt-in-shamir-secret-sharing
pub fn split_seed_2of3(seed: &[u8; 32]) -> Vec<Vec<u8>> {
    let sharks = sharks::Sharks(2);
    let dealer = sharks.dealer(seed);
    dealer.take(3).map(|s| Vec::from(&s)).collect()
}

/// Reconstruct a seed from a set of Shamir shares (≥ 2 of the 3).
pub fn recover_seed(shares: &[Vec<u8>]) -> Result<Vec<u8>, BackupError> {
    let sharks = sharks::Sharks(2);
    let parsed: Result<Vec<sharks::Share>, _> = shares
        .iter()
        .map(|b| sharks::Share::try_from(b.as_slice()))
        .collect();
    let parsed = parsed.map_err(|_| BackupError::Format("malformed Shamir share".into()))?;
    sharks
        .recover(parsed.iter())
        .map_err(|e| BackupError::Format(format!("Shamir recover: {e}")))
}

/// Draw a fresh random 32-byte recovery seed.
pub fn new_recovery_seed() -> [u8; 32] {
    rng::random_array::<32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_verification_tag_compare() {
        let master = [0x42u8; 32];
        // Fast params for the test.
        let blob = pwkdf::wrap_with(
            &master,
            b"correct horse battery staple",
            crate::crypto::primitives::Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap();
        // Correct secret against the matching device master → Verified.
        assert_eq!(
            verify_recovery_secret(&blob, b"correct horse battery staple", &master),
            VerifyOutcome::Verified
        );
        // Wrong secret → NotVerified (caller applies the refresh-then-retry rule).
        assert_eq!(
            verify_recovery_secret(&blob, b"wrong", &master),
            VerifyOutcome::NotVerified
        );
        // Right secret, different device master (rotated elsewhere) → NotVerified.
        assert_eq!(
            verify_recovery_secret(&blob, b"correct horse battery staple", &[0x43u8; 32]),
            VerifyOutcome::NotVerified
        );
    }

    #[test]
    fn master_key_escrow_round_trip() {
        let master = [0x42u8; 32];
        // Fast params for the test.
        let blob = pwkdf::wrap_with(
            &master,
            b"recovery passphrase",
            crate::crypto::primitives::Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap();
        assert_eq!(
            recover_master_key(&blob, b"recovery passphrase").unwrap(),
            master
        );
        assert!(recover_master_key(&blob, b"wrong").is_err());
    }

    #[test]
    fn shamir_2_of_3_reconstructs_from_any_two() {
        let seed = [0x7u8; 32];
        let shares = split_seed_2of3(&seed);
        assert_eq!(shares.len(), 3);

        // Any 2 of the 3 reconstruct the seed.
        for pair in [[0, 1], [0, 2], [1, 2]] {
            let subset = vec![shares[pair[0]].clone(), shares[pair[1]].clone()];
            assert_eq!(recover_seed(&subset).unwrap(), seed);
        }
    }

    #[test]
    fn shamir_single_share_does_not_reconstruct() {
        let seed = [0x9u8; 32];
        let shares = split_seed_2of3(&seed);
        // A single share is below threshold → cannot recover the seed.
        let one = vec![shares[0].clone()];
        match recover_seed(&one) {
            Err(_) => {}
            Ok(v) => assert_ne!(v, seed, "one share must not reveal the seed"),
        }
    }
}
