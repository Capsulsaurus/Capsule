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
    BackupArtifact, BackupAsset, BackupDoc, BackupDocKind, BackupInput, CiphertextSource,
    RestoreMode, RestoreReport, RestoredAsset, WRAP_PARAMS, export, export_to, export_with_salt,
};
/// The caller-chosen-cost export entry points. Gated identically to their definitions: a weak
/// Argon2id cost yields a brute-forceable artifact, so reaching one takes `cfg(test)` or an
/// explicit `test-support` line in the consuming manifest.
#[cfg(any(test, feature = "test-support"))]
pub use artifact::{export_with_params, export_with_params_to, export_with_salt_and_params};
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
///
/// Format 2 likewise carries an optional `library/attestation-keys.cbor` entry: the library's
/// pinned attestation-key history, verbatim (#805). Additive again: an older reader hash-checks
/// it and ignores it, and an artifact without it restores as before.
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
    /// The artifact pins an attestation-key history for `origin` that diverges from the one the
    /// target library pins: each holds a key the other lacks. Neither is overwritten, and the
    /// restore is refused before it writes anything.
    #[error(
        "backup attestation-key pin for {origin} diverges from the library's: {only_carried} \
         key(s) only in the backup, {only_local} only in the library"
    )]
    AttestationKeyPinsDiverge {
        /// The canonical origin whose two pinned histories diverge.
        origin: String,
        /// How many keys only the artifact's pin holds.
        only_carried: usize,
        /// How many keys only the library's pin holds.
        only_local: usize,
    },
    /// The artifact's pinned attestation-key history could not be read, decoded or restored.
    #[error("backup attestation-key pin: {0}")]
    AttestationKeyPins(String),
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

// ── Recovery verification cadence ───────────────────────────────────

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
/// cadence, snooze, and refresh-before-fail policy live in the client.
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
mod tests;
