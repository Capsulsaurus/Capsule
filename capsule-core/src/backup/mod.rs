//! The portable backup artifact and recovery mechanisms (SSoT: [Backup and Recovery]).
//!
//! Two distinct things are kept separate:
//! - the **backup artifact** — a deterministic, self-describing, signed tar of a
//!   library's encrypted blobs, metadata blobs, provenance chains, the AMK ledger needed
//!   to decrypt them, and each album's public authority ledger needed to verify them;
//! - the **[master-key escrow](escrow_master_key)** — a small passphrase-wrapped blob that
//!   reconstructs the key hierarchy.
//!
//! Recovery rests on one rule: holding the recovery secret restores every asset, even after
//! every device is lost. The artifact carries its own AMK ledger, so it is self-sufficient.
//!
//! [Backup and Recovery]: https://docs/design/backup-recovery/

mod artifact;

pub use artifact::{
    BackupArtifact, BackupAsset, BackupDoc, BackupDocKind, BackupInput, RestoreMode, RestoreReport,
    RestoredAsset, WRAP_PARAMS, export, export_with_salt,
};
/// The caller-chosen-cost export entry points. Gated identically to their definitions: a weak
/// Argon2id cost yields a brute-forceable artifact, so reaching one takes `cfg(test)` or an
/// explicit `test-support` line in the consuming manifest.
#[cfg(any(test, feature = "test-support"))]
pub use artifact::{export_with_params, export_with_salt_and_params};
use thiserror::Error;

use crate::crypto::primitives::DeviceTier;
use crate::crypto::pwkdf::{self, WrappedSecret};
use crate::crypto::{CryptoError, kdf, rng};

/// The backup artifact format version.
///
/// - `1` — blobs, metadata, provenance, receipts, and the sealed AMK ledger.
/// - `2` — adds an `authority/{album_id}` entry per album: its public admin-signed epoch
///   ledger, so a restored asset verifies (#468). A format-1 artifact still opens; its albums
///   restore readable but unverifiable. A format newer than this build is refused.
///
/// Format 2 also carries optional `docs/{album|stack}/{id}` entries: each album's and stack's
/// label record, sealed under its album's AMK (#667). Additive, so no bump: an older format-2
/// reader verifies them as manifest entries and otherwise ignores them, restoring the album as
/// `Recovered album` exactly as before.
///
/// Format 2 likewise carries an optional `attestations/{asset_id}` entry per asset: its signed
/// storage-attestation log, verbatim, as `receipts/{asset_id}` carries its receipt log (#747).
/// Additive for the same reason: an older reader hash-checks it as a manifest entry and
/// restores the asset without it.
pub const ARTIFACT_FORMAT_VERSION: u16 = 2;

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
/// Each share is `x ‖ y` ([`SHAMIR_SHARE_LEN`] bytes), the encoding fixed by
/// [Backup — Share Wire Format]; shares issued before the `sharks` → `blahaj` swap use the
/// same encoding and recover unchanged.
///
/// [Backup — Opt-in: Shamir Secret Sharing]: https://docs/design/backup-recovery/#opt-in-shamir-secret-sharing
/// [Backup — Share Wire Format]: https://docs/design/backup-recovery/#share-wire-format
pub fn split_seed_2of3(seed: &[u8; 32]) -> Vec<Vec<u8>> {
    let dealer = blahaj::Sharks(SHAMIR_THRESHOLD).dealer(seed);
    let shares: Vec<Vec<u8>> = dealer.take(3).map(|s| Vec::from(&s)).collect();
    tracing::debug!(
        threshold = SHAMIR_THRESHOLD,
        shares = shares.len(),
        "shamir: split recovery seed"
    );
    shares
}

/// Shares needed to reconstruct the seed.
const SHAMIR_THRESHOLD: u8 = 2;

/// Encoded length of one share of the 32-byte seed: the evaluation point `x`, then 32 `y` bytes.
pub const SHAMIR_SHARE_LEN: usize = 33;

/// Reconstruct a seed from a set of Shamir shares (≥ 2 of the 3).
///
/// Every share must be [`SHAMIR_SHARE_LEN`] bytes with a nonzero `x`; fewer than two distinct
/// `x` values is below threshold. Each is a [`BackupError::Format`].
pub fn recover_seed(shares: &[Vec<u8>]) -> Result<Vec<u8>, BackupError> {
    let parsed = shares
        .iter()
        .map(|b| {
            if b.len() != SHAMIR_SHARE_LEN {
                return Err(BackupError::Format(format!(
                    "malformed Shamir share: {} bytes, expected {SHAMIR_SHARE_LEN}",
                    b.len()
                )));
            }
            if b[0] == 0 {
                return Err(BackupError::Format(
                    "malformed Shamir share: evaluation point 0".into(),
                ));
            }
            blahaj::Share::try_from(b.as_slice())
                .map_err(|e| BackupError::Format(format!("malformed Shamir share: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Two shares at the same `x` with different `y` would interpolate to a wrong seed rather
    // than fail, so a repeated evaluation point is refused outright.
    for (i, share) in shares.iter().enumerate() {
        if shares[..i].iter().any(|prior| prior[0] == share[0]) {
            return Err(BackupError::Format(format!(
                "Shamir recover: duplicate evaluation point {}",
                share[0]
            )));
        }
    }
    let seed = blahaj::Sharks(SHAMIR_THRESHOLD)
        .recover(parsed.iter())
        .map_err(|e| BackupError::Format(format!("Shamir recover: {e}")))?;
    tracing::debug!(shares = parsed.len(), "shamir: recovered seed");
    Ok(seed)
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

    /// Shares `sharks` 0.5.0 issued for the seed `[0, 1, …, 31]`, captured before the swap to
    /// `blahaj`. They pin the wire-format decision: shares already handed out stay recoverable.
    const SHARKS_ISSUED: [[u8; SHAMIR_SHARE_LEN]; 3] = [
        [
            0x01, 0xa1, 0x62, 0x17, 0xb5, 0x53, 0xf7, 0xdb, 0x50, 0x81, 0x0e, 0x9e, 0x13, 0xdf,
            0x78, 0x76, 0xa8, 0x44, 0x35, 0xeb, 0x3a, 0x8a, 0xce, 0xf9, 0x81, 0x0f, 0x98, 0x87,
            0x6b, 0x39, 0x05, 0x1f, 0xe9,
        ],
        [
            0x02, 0x5f, 0xc7, 0x28, 0x72, 0xaa, 0xfc, 0xa1, 0xa9, 0x07, 0x07, 0x3f, 0x3b, 0xb7,
            0xe7, 0xfe, 0x5c, 0xb8, 0x59, 0xfd, 0x41, 0x35, 0xbe, 0xd5, 0x26, 0x36, 0x06, 0x3d,
            0xfb, 0x56, 0x2d, 0x1c, 0xee,
        ],
        [
            0x03, 0xfe, 0xa4, 0x3d, 0xc4, 0xfd, 0x0e, 0x7c, 0xfe, 0x8e, 0x00, 0xab, 0x23, 0x64,
            0x92, 0x86, 0xfb, 0xec, 0x7d, 0x04, 0x68, 0xab, 0x65, 0x3a, 0xb0, 0x21, 0x87, 0xa0,
            0x8b, 0x73, 0x35, 0x1d, 0x18,
        ],
    ];

    #[test]
    fn shamir_recovers_shares_issued_by_sharks() {
        let seed: Vec<u8> = (0..32).collect();
        for pair in [[0, 1], [0, 2], [1, 2], [2, 0]] {
            let subset = vec![
                SHARKS_ISSUED[pair[0]].to_vec(),
                SHARKS_ISSUED[pair[1]].to_vec(),
            ];
            assert_eq!(recover_seed(&subset).unwrap(), seed, "pair {pair:?}");
        }
        let all: Vec<Vec<u8>> = SHARKS_ISSUED.iter().map(|s| s.to_vec()).collect();
        assert_eq!(recover_seed(&all).unwrap(), seed);
    }

    #[test]
    fn shamir_shares_use_the_documented_encoding() {
        let shares = split_seed_2of3(&[0x5u8; 32]);
        for (i, share) in shares.iter().enumerate() {
            assert_eq!(share.len(), SHAMIR_SHARE_LEN);
            assert_eq!(usize::from(share[0]), i + 1, "x runs 1, 2, 3");
        }
    }

    /// RUSTSEC-2024-0398: `sharks` drew the degree-1 coefficient from `[1, 255]`, so share
    /// `x = 1` (`y = a + s`) never equalled the seed byte. Uniform coefficients hit it at rate
    /// 1/256; over 8192 bytes the chance of never hitting it is about e^-32.
    #[test]
    fn shamir_coefficients_include_zero() {
        let seed = [0u8; 32];
        let hit = (0..256).any(|_| split_seed_2of3(&seed)[0][1..].contains(&0));
        assert!(
            hit,
            "a degree-1 coefficient of 0 never occurred: biased dealer"
        );
    }

    #[test]
    fn shamir_single_share_does_not_reconstruct() {
        let shares = split_seed_2of3(&[0x9u8; 32]);
        for share in &shares {
            assert!(matches!(
                recover_seed(std::slice::from_ref(share)),
                Err(BackupError::Format(_))
            ));
        }
        assert!(matches!(recover_seed(&[]), Err(BackupError::Format(_))));
    }

    #[test]
    fn shamir_rejects_malformed_shares() {
        let shares = split_seed_2of3(&[0x3u8; 32]);
        let good = shares[1].clone();
        let mut x_zero = shares[0].clone();
        x_zero[0] = 0;
        let cases: [(&str, Vec<Vec<u8>>); 5] = [
            ("empty share", vec![Vec::new(), good.clone()]),
            ("x only", vec![vec![1], good.clone()]),
            ("truncated", vec![shares[0][..32].to_vec(), good.clone()]),
            (
                "overlong",
                vec![[shares[0].as_slice(), &[0]].concat(), good.clone()],
            ),
            ("x = 0", vec![x_zero, good.clone()]),
        ];
        for (name, input) in cases {
            assert!(
                matches!(recover_seed(&input), Err(BackupError::Format(_))),
                "{name} must be refused"
            );
        }
    }

    #[test]
    fn shamir_rejects_a_repeated_evaluation_point() {
        let shares = split_seed_2of3(&[0x4u8; 32]);
        // The same share twice is one share: below threshold.
        let twice = vec![shares[0].clone(), shares[0].clone()];
        assert!(matches!(recover_seed(&twice), Err(BackupError::Format(_))));
        // A forged share at an existing x would otherwise interpolate a wrong seed.
        let mut forged = shares[0].clone();
        forged[1] ^= 0xff;
        let colliding = vec![shares[0].clone(), forged, shares[1].clone()];
        assert!(matches!(
            recover_seed(&colliding),
            Err(BackupError::Format(_))
        ));
    }
}
