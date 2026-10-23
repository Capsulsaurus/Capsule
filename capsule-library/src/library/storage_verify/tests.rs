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

// ─── Wiring: the gate over the `StorageVerifier` seam ──────────────────

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

/// The acceptance: the device-owned-original release, Move-import source deletion, and
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
