//! The verify-before-destroy gate — client half of storage verification (slices `S-C3`
//! server endpoint / `S-D4` client wiring in the repo-root `SLICES.md`; SSoT:
//! [Import — Storage Verification](https://docs/design/import/storage-verification/)).
//!
//! Before any post-write local cleanup of irreplaceable bytes (releasing a device-owned
//! original, deleting a Move-import source, a streaming-mode release), a client requires
//! **both** halves to pass: `verify_asset` accepts the asset (crypto validity — the
//! offline core already implements it) and the server's `POST /storage/verify` verdict is
//! `durable` (stored ∧ indexed ∧ retrievable for every required blob). The predicate here
//! is the pure conjunction those call sites consume; fetching the verdict is `S-D4`.

use std::fs;
use std::path::Path;

use uuid::Uuid;

use crate::crypto::hash::Hash32;
/// A blob's role within an asset, as the storage-verification endpoint reports it.
///
/// Defined in [`crate::crypto::receipts`] and re-exported here, because a custody receipt's
/// `blob_role` is written from the same enum and the issuer and the verifier must not each
/// have their own (`S-C46`). This path is the one the storage-verification doc names, so it
/// stays.
pub use crate::crypto::receipts::BlobRole;
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
/// before any local cleanup, and streaming import (`S-B3`) drives its per-asset release window
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
/// originals, which [`cache_sweep`](crate::library::cache::cache_sweep) never touches
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
/// unlinked only on `Release`. This is the seam [streaming import](../../import/index.html)
/// (`S-B3`) drives per asset in its import→upload→verify→release window.
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
mod tests {
    use super::*;

    fn blob(stored: bool, indexed: bool, retrievable: bool) -> BlobVerdict {
        BlobVerdict {
            hash: Hash32([0xAA; 32]),
            role: BlobRole::Original,
            stored,
            indexed,
            retrievable,
        }
    }

    fn verdict(durable: bool, blobs: Vec<BlobVerdict>) -> StorageVerdict {
        StorageVerdict {
            asset_id: Uuid::from_u128(1),
            durable,
            blobs,
            checked_at: "2026-07-02T00:00:00Z".into(),
        }
    }

    #[test]
    fn release_requires_both_halves_and_consistent_details() {
        let good = verdict(true, vec![blob(true, true, true)]);
        assert!(release_is_safe(&good, true));
        // Crypto half failed: never release.
        assert!(!release_is_safe(&good, false));
        // Server says durable but a detail row disagrees: the client's re-check wins.
        let inconsistent = verdict(true, vec![blob(true, true, false)]);
        assert!(!release_is_safe(&inconsistent, true));
        // Not durable: never release.
        assert!(!release_is_safe(
            &verdict(false, vec![blob(true, true, true)]),
            true
        ));
        // An empty verdict confirms nothing.
        assert!(!release_is_safe(&verdict(true, vec![]), true));
    }

    // ─── S-D4 wiring: the gate over the `StorageVerifier` seam ──────────────────

    use std::cell::Cell;

    use tempfile::TempDir;

    use crate::db::rows::CachedRepresentationRow;

    /// A deterministic stand-in for the SDK's `POST /storage/verify` + receipt fetch: it
    /// returns a canned verdict and receipt-verified fact, and records the hashes it was asked
    /// about so a test can assert the destructive path actually consulted the server.
    struct MockVerifier {
        verdict: Result<StorageVerdict, ()>,
        receipt_verified: Result<bool, ()>,
        asked: Cell<Option<Vec<Hash32>>>,
    }

    impl MockVerifier {
        fn new(durable: bool, receipt_verified: bool) -> Self {
            let blobs = vec![blob(durable, durable, durable)];
            Self {
                verdict: Ok(StorageVerdict {
                    asset_id: Uuid::from_u128(1),
                    durable,
                    blobs,
                    checked_at: "2026-07-10T00:00:00Z".into(),
                }),
                receipt_verified: Ok(receipt_verified),
                asked: Cell::new(None),
            }
        }
    }

    impl StorageVerifier for MockVerifier {
        fn verify(
            &self,
            _asset_id: Uuid,
            blob_hashes: &[Hash32],
        ) -> Result<StorageVerdict, VerifierError> {
            self.asked.set(Some(blob_hashes.to_vec()));
            self.verdict
                .clone()
                .map_err(|()| VerifierError::Transport("mock".into()))
        }

        fn receipt_verified(
            &self,
            _asset_id: Uuid,
            _blob_hashes: &[Hash32],
        ) -> Result<bool, VerifierError> {
            self.receipt_verified
                .map_err(|()| VerifierError::Transport("mock".into()))
        }
    }

    /// `S-D4` acceptance: the device-owned-original release, Move-import source deletion, and
    /// streaming-mode release all gate on [`ReleaseGate::may_release`] fed by a
    /// `POST /storage/verify` verdict + custody receipt; a non-`durable` verdict, a missing
    /// receipt, or a transport failure each retains the local copy and surfaces the reason.
    #[test]
    fn destructive_paths_gate_on_release_is_safe() {
        let asset = Uuid::from_u128(1);
        let hashes = [Hash32([0xAA; 32])];

        // 1. Device-owned-original release: only a durable+receipt verdict deletes the file and
        //    drops the owned-original row; a non-durable verdict leaves both intact.
        for (durable, receipt, expect_release) in [
            (true, true, true),
            (false, true, false),
            (true, false, false),
        ] {
            let dir = TempDir::new().unwrap();
            let original = dir.path().join("owned.jpg");
            std::fs::write(&original, b"only copy").unwrap();
            let db = DatabaseDriver::open_in_memory().unwrap();
            db.upsert_representation(&CachedRepresentationRow {
                uuid: asset.to_string(),
                tier: "original".into(),
                format: Some("jpg".into()),
                bytes: 9,
                path: original.to_string_lossy().into_owned(),
                last_accessed_at: 1,
                pinned: false,
                is_owned_original: true,
            })
            .unwrap();

            let verifier = MockVerifier::new(durable, receipt);
            let decision =
                release_owned_original(&db, &verifier, asset, &original, &hashes, true).unwrap();
            assert_eq!(decision.is_release(), expect_release);
            assert_eq!(original.exists(), !expect_release, "owned original file");
            assert_eq!(
                db.representations_for(&asset.to_string())
                    .unwrap()
                    .is_empty(),
                expect_release,
                "owned-original row"
            );
            // The gate consulted the server with the exact hashes the client declared.
            assert_eq!(verifier.asked.take().as_deref(), Some(&hashes[..]));
        }

        // 2. Move-import source deletion: identical gating over the external source path.
        {
            let dir = TempDir::new().unwrap();
            let src = dir.path().join("move_me.jpg");
            std::fs::write(&src, b"external").unwrap();
            // Non-durable → source retained.
            let retain =
                release_move_source(&MockVerifier::new(false, true), asset, &src, &hashes, true);
            assert_eq!(retain, ReleaseDecision::Retain(RetainReason::NotDurable));
            assert!(src.exists());
            // Durable + receipt → source deleted.
            let release =
                release_move_source(&MockVerifier::new(true, true), asset, &src, &hashes, true);
            assert!(release.is_release());
            assert!(!src.exists());
        }

        // 3. Streaming-mode release drives the same gate; a transport failure never destroys.
        {
            struct FailVerifier;
            impl StorageVerifier for FailVerifier {
                fn verify(&self, _: Uuid, _: &[Hash32]) -> Result<StorageVerdict, VerifierError> {
                    Err(VerifierError::Transport("down".into()))
                }
                fn receipt_verified(&self, _: Uuid, _: &[Hash32]) -> Result<bool, VerifierError> {
                    Ok(true)
                }
            }
            let decision = ReleaseGate::new(&FailVerifier).may_release(asset, &hashes, true);
            assert_eq!(
                decision,
                ReleaseDecision::Retain(RetainReason::VerifyUnavailable)
            );
        }

        // crypto half (verify_asset rejected) never releases even on a durable+receipt verdict.
        let decision =
            ReleaseGate::new(&MockVerifier::new(true, true)).may_release(asset, &hashes, false);
        assert_eq!(decision, ReleaseDecision::Retain(RetainReason::NotDurable));
    }
}
