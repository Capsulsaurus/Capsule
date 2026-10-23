//! The verify-before-destroy gate — client half of storage verification (SSoT:
//! [Transfer — Durability](https://docs/design/transfer/durability/)).
//!
//! Before any post-write local cleanup of irreplaceable bytes (releasing a device-owned
//! original, deleting a Move-import source, a streaming-mode release), a client requires
//! **both** halves to pass: `verify_asset` accepts the asset (crypto validity — the
//! offline core already implements it) and the server's `POST /storage/verify` verdict is
//! `durable` (stored ∧ indexed ∧ retrievable for every required blob). The predicate here
//! is the pure conjunction those call sites consume; fetching the verdict goes through the
//! `StorageVerifier` seam.

use std::fs;
use std::path::Path;

use capsule_core::crypto::hash::Hash32;
/// A blob's role within an asset, as the storage-verification endpoint reports it.
///
/// Defined in [`capsule_core::crypto::receipts`] and re-exported here, because a custody receipt's
/// `blob_role` is written from the same enum and the issuer and the verifier must not each
/// have their own. This path is the one the transfer durability doc names, so it
/// stays.
pub use capsule_core::crypto::receipts::BlobRole;
use uuid::Uuid;

use crate::db::DatabaseDriver;

/// The server's key-free per-blob verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobVerdict {
    /// The content address the client declared it relies on.
    pub hash: Hash32,
    /// The blob's role on the asset.
    pub role: BlobRole,
    /// Present in the blob store at its content address (`stat`), not merely in-flight.
    pub stored: bool,
    /// Referenced by a committed, `uploaded = true` index row.
    pub indexed: bool,
    /// Refcount > 0, not mid-GC (`collectable_since`), not quarantined.
    pub retrievable: bool,
}

impl BlobVerdict {
    /// One blob's contribution to durability: all three independent facts hold.
    pub fn safely_stored(&self) -> bool {
        self.stored && self.indexed && self.retrievable
    }
}

/// The per-asset verdict from `POST /storage/verify`. `durable` attests **this home
/// server's** storage only — never replicas or peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageVerdict {
    /// The asset the verdict is for.
    pub asset_id: Uuid,
    /// Server-computed: every required blob is stored ∧ indexed ∧ retrievable.
    pub durable: bool,
    /// Per-blob detail, one entry per hash the client declared.
    pub blobs: Vec<BlobVerdict>,
    /// The server's trusted clock at verification (RFC 3339, like `received_at`).
    pub checked_at: String,
}

/// The verify-before-destroy predicate: destructive local cleanup of irreplaceable bytes
/// may proceed **only** when the server's verdict is `durable`, every declared blob
/// individually re-checks as safely stored (the client never trusts the server's
/// aggregate over the details it can recompute), and `verify_asset` accepted the asset.
///
/// A `false` result never triggers a destructive action — the caller retains the local
/// copy, retries with backoff, and surfaces "not yet confirmed on server".
pub fn release_is_safe(verdict: &StorageVerdict, verify_asset_accepted: bool) -> bool {
    verify_asset_accepted
        && verdict.durable
        && !verdict.blobs.is_empty()
        && verdict.blobs.iter().all(BlobVerdict::safely_stored)
}

// ─── The release gate seam ────────────────────────────────────────────────────

/// A transport error fetching a verdict or receipt from the server. A gate that cannot reach a
/// definitive `durable`/receipt answer **never** destroys — the caller retains and retries.
#[derive(Debug, thiserror::Error)]
pub enum VerifierError {
    /// The `POST /storage/verify` or receipt fetch failed on the wire or server-side.
    #[error("storage verifier transport: {0}")]
    Transport(String),
}

/// The client's source of the server-side facts the gate needs: the key-free `/storage/verify`
/// verdict and whether a **verified** custody receipt is held for the write. The `capsule-sdk`
/// fills this over HTTP (`POST /storage/verify` + `GET …/receipt` verified under the pinned
/// attestation key); tests fill it with deterministic mocks. Sync by construction — the offline
/// data plane calls it; async callers resolve the futures before invoking the gate.
pub trait StorageVerifier {
    /// Fetch a fresh `/storage/verify` verdict for the asset's declared blob hashes.
    fn verify(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<StorageVerdict, VerifierError>;

    /// Whether a custody receipt covering the write is held **and verifies** under the pinned
    /// attestation key with fields matching what the client sent. `false` (not an error) means
    /// missing-or-unverified, which the gate treats as refuse-to-release.
    fn receipt_verified(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<bool, VerifierError>;
}

/// Why the gate refused to release. Every variant retains the local bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainReason {
    /// The server's verdict was not `durable` (or a per-blob re-check disagreed, or
    /// `verify_asset` rejected the asset).
    NotDurable,
    /// No verified custody receipt is held for the write (missing or unverified).
    ReceiptMissing,
    /// The verdict fetch failed on the wire — retry with backoff.
    VerifyUnavailable,
    /// The receipt fetch failed on the wire — retry with backoff.
    ReceiptUnavailable,
    /// The read-back check failed: the library copy, or the Move source itself, no longer
    /// hashes to the asset's signed original. Deleting the source could lose the only copy of
    /// those bytes, so it is kept whatever the server says.
    ReadBackMismatch,
}

/// The gate's decision. `Release` is returned **only** when all three checks pass (durable
/// verdict, matching per-blob detail + `verify_asset`, and a verified receipt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseDecision {
    /// Every check passed; destroying the local bytes is safe.
    Release,
    /// A check did not pass; the local bytes are retained.
    Retain(RetainReason),
}

impl ReleaseDecision {
    /// Whether this decision permits destroying the local bytes.
    #[must_use]
    pub fn is_release(&self) -> bool {
        matches!(self, ReleaseDecision::Release)
    }
}

/// The verify-before-destroy gate over a [`StorageVerifier`]. It never destroys anything itself:
/// [`may_release`](Self::may_release) is the pure decision the three destructive paths
/// (device-owned-original release, Move-import source deletion, streaming-mode release) consult
/// before any local cleanup, and streaming import drives its per-asset release window
/// through the same call.
pub struct ReleaseGate<'a, V: StorageVerifier + ?Sized> {
    verifier: &'a V,
}

impl<'a, V: StorageVerifier + ?Sized> ReleaseGate<'a, V> {
    /// Build a gate over a verifier.
    pub fn new(verifier: &'a V) -> Self {
        Self { verifier }
    }

    /// Decide whether the write's local bytes may be released. Both halves of the gate are
    /// required: the server's verdict must be `durable` (with every declared blob individually
    /// re-checking as safely stored and `verify_asset` having accepted the asset — the
    /// [`release_is_safe`] conjunction), **and** a verified custody receipt must be held. A
    /// transport failure on either fetch yields a `Retain`, never a destroy.
    pub fn may_release(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
        verify_asset_accepted: bool,
    ) -> ReleaseDecision {
        let Ok(verdict) = self.verifier.verify(asset_id, blob_hashes) else {
            return ReleaseDecision::Retain(RetainReason::VerifyUnavailable);
        };
        if !release_is_safe(&verdict, verify_asset_accepted) {
            return ReleaseDecision::Retain(RetainReason::NotDurable);
        }
        match self.verifier.receipt_verified(asset_id, blob_hashes) {
            Ok(true) => ReleaseDecision::Release,
            Ok(false) => ReleaseDecision::Retain(RetainReason::ReceiptMissing),
            Err(_) => ReleaseDecision::Retain(RetainReason::ReceiptUnavailable),
        }
    }
}

// ─── The three destructive paths, gated ───────────────────────────────────────

/// **Device-owned-original release** (the cache-eviction sweep's counterpart for owned
/// originals, which [`cache_sweep`](crate::library::cache_sweep) never touches
/// automatically). An original the device itself uploaded is the source of truth until the
/// server durably holds it; it is released — its local file deleted and its owned-original
/// representation row dropped, after which it becomes an ordinary server-only, re-fetchable
/// asset — **only** on a `Release` decision. On any `Retain` the file and row are untouched.
pub fn release_owned_original<V: StorageVerifier + ?Sized>(
    db: &DatabaseDriver,
    verifier: &V,
    asset_id: Uuid,
    local_path: &Path,
    blob_hashes: &[Hash32],
    verify_asset_accepted: bool,
) -> Result<ReleaseDecision, rusqlite::Error> {
    let decision =
        ReleaseGate::new(verifier).may_release(asset_id, blob_hashes, verify_asset_accepted);
    if decision.is_release() {
        let _ = fs::remove_file(local_path);
        db.remove_representation(&asset_id.to_string(), "original")?;
    }
    Ok(decision)
}

/// **Move-import source deletion.** Deleting the external source after a Move-mode import waits
/// on the server's `durable` verdict + a verified receipt — never on the local library copy
/// alone — so a crash mid-import cannot lose the only copy. Returns the decision; the source is
/// unlinked only on `Release`. The gate alone: a caller holding the library uses
/// [`Workspace::release_move_source`](crate::lifecycle::Workspace::release_move_source), which
/// adds the read-back hash check before it reaches here.
pub fn release_move_source<V: StorageVerifier + ?Sized>(
    verifier: &V,
    asset_id: Uuid,
    src_path: &Path,
    blob_hashes: &[Hash32],
    verify_asset_accepted: bool,
) -> ReleaseDecision {
    let decision =
        ReleaseGate::new(verifier).may_release(asset_id, blob_hashes, verify_asset_accepted);
    if decision.is_release() {
        let _ = fs::remove_file(src_path);
    }
    decision
}

#[cfg(test)]
mod tests;
