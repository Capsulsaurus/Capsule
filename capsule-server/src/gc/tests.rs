//! The collector's own suite.
//!
//! Every case here is really the same question from a different angle: *would this pass have
//! deleted bytes something still needs?* The mark, the window, the re-confirmation and the
//! dangling arm all exist to make the answer no, so the suite is weighted toward the cases
//! where a naive collector would say yes.

use std::sync::Arc;

use capsule_core::crypto::hash::{Hash32, hash_bytes};

use super::memory::InMemoryCollection;
use super::*;
use crate::blob::{BlobStore, InMemoryBlobStore};
use crate::index::memory::InMemoryAssetIndex;
use crate::index::{
    AssetIndex, BlobRecord, LifecycleOp, OpAction, OpOutcome, PendingAsset, Reservation,
};
use crate::store::{AlbumId, BlobRole, Clock, OwnerId, SystemClock};

/// A test clock that starts at the epoch and moves when told.
#[derive(Debug, Default)]
struct StepClock(std::sync::Mutex<Timestamp>);

impl StepClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(std::sync::Mutex::new(Timestamp::UNIX_EPOCH)))
    }

    fn advance(&self, by: SignedDuration) {
        let mut now = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now += by;
    }
}

impl crate::store::Clock for StepClock {
    fn now(&self) -> Timestamp {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The four collaborators plus the clock, assembled.
struct Harness {
    context: CollectionContext,
    index: Arc<InMemoryAssetIndex>,
    blobs: Arc<InMemoryBlobStore>,
    marks: Arc<InMemoryCollection>,
    quotas: Arc<crate::quota::InMemoryQuota>,
    clock: Arc<StepClock>,
}

impl Harness {
    fn new() -> Self {
        let index = Arc::new(InMemoryAssetIndex::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let marks = Arc::new(InMemoryCollection::new());
        let quotas = Arc::new(crate::quota::InMemoryQuota::new());
        let clock = StepClock::new();
        let context = CollectionContext::new(
            index.clone(),
            blobs.clone(),
            marks.clone(),
            quotas.clone(),
            clock.clone(),
            DEFAULT_GRACE_WINDOW,
        );
        Self {
            context,
            index,
            blobs,
            marks,
            quotas,
            clock,
        }
    }

    /// Put `bytes` in the store and return their address.
    async fn store(&self, bytes: &[u8]) -> ContentAddress {
        let address = ContentAddress::parse(&hash_bytes(bytes).to_hex()).expect("an address");
        self.blobs.put(&address, bytes).await.expect("stored");
        address
    }

    /// Publish `asset` holding `bytes` as a derivative, and return the address.
    async fn publish(&self, asset: &str, bytes: &[u8]) -> ContentAddress {
        let id = AssetId::new(asset);
        let reserved = self
            .index
            .reserve(PendingAsset {
                asset_id: id.clone(),
                owner_id: OwnerId::new("gc-owner"),
                album_id: AlbumId::new("gc-album"),
                protocol_version: "2026-01-01".to_owned(),
                crypto_suite_id: 1,
                created_at: Timestamp::UNIX_EPOCH,
            })
            .await
            .expect("the index reserves");
        assert!(matches!(reserved, Reservation::Created(_)));

        for (role, seed) in [
            (BlobRole::Provenance, format!("{asset}-provenance")),
            (BlobRole::Metadata, format!("{asset}-metadata")),
        ] {
            let address = self.store(seed.as_bytes()).await;
            self.record(&id, role, &address).await;
        }
        let address = self.store(bytes).await;
        self.record(&id, BlobRole::Derivative, &address).await;
        address
    }

    async fn record(&self, asset: &AssetId, role: BlobRole, address: &ContentAddress) {
        self.index
            .record_blob(
                asset,
                BlobRecord {
                    role,
                    address: address.clone(),
                    size: 16,
                    manifest_sha256: None,
                    finalized_at: Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }

    /// Tombstone `asset` with a retention floor.
    async fn delete(&self, asset: &str, retention_until: Option<Timestamp>, seed: u8) {
        let outcome = self
            .index
            .apply_op(LifecycleOp {
                asset_id: AssetId::new(asset),
                owner_id: OwnerId::new("gc-owner"),
                album_id: AlbumId::new("gc-album"),
                action: OpAction::Delete,
                manifest_hash: Hash32([seed; 32]),
                prior_provenance_hash: self
                    .index
                    .read(&AssetId::new(asset))
                    .await
                    .expect("read")
                    .expect("the row exists")
                    .chain_head,
                amk_version: 1,
                provenance: self
                    .store(format!("{asset}-delete-manifest").as_bytes())
                    .await,
                original: None,
                metadata: None,
                retention_until,
                at: self.clock.now(),
            })
            .await
            .expect("the index applies");
        assert!(matches!(outcome, OpOutcome::Applied { .. }));
    }
}

use crate::quota::QuotaStore as _;

impl Harness {
    /// Charge `address` to `user` so a later sweep has something to credit back.
    async fn charge(&self, user: &str, address: &ContentAddress, size: u64) {
        let outcome = self
            .quotas
            .charge(
                &crate::store::UserId::new(user),
                address,
                size,
                self.clock.now(),
                crate::quota::QuotaLimits::unlimited(),
            )
            .await
            .expect("the ledger charges");
        assert!(matches!(
            outcome,
            crate::quota::ChargeOutcome::Charged { .. }
        ));
    }

    /// What `user` currently owes.
    async fn used(&self, user: &str) -> u64 {
        self.quotas
            .usage(&crate::store::UserId::new(user))
            .await
            .expect("the ledger answers")
            .used
    }
}

/// A day, for readability.
fn days(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n * 24)
}

// ===========================================================================================

#[tokio::test]
async fn a_referenced_blob_is_never_marked() {
    let h = Harness::new();
    h.publish("live", b"referenced bytes").await;

    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(
        report.marked.is_empty(),
        "every blob in the store belongs to a live asset, so nothing is collectable"
    );
    assert!(report.swept.is_empty());
}

#[tokio::test]
async fn an_orphan_is_marked_then_swept_after_the_window() {
    let h = Harness::new();
    let orphan = h.store(b"a finalization-crash orphan").await;

    // First pass: zero references, so it is marked and nothing else.
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert_eq!(report.marked, vec![orphan.clone()]);
    assert!(report.swept.is_empty(), "a mark is not a sweep");
    assert!(h.blobs.stat(&orphan).await.expect("stat").is_some());

    // Inside the window: still nothing.
    h.clock.advance(days(1) - SignedDuration::from_hours(1));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(report.marked.is_empty());
    assert!(report.swept.is_empty());

    // Past it: swept, and the mark goes with the bytes.
    h.clock.advance(SignedDuration::from_hours(2));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert_eq!(report.swept, vec![orphan.clone()]);
    assert!(h.blobs.stat(&orphan).await.expect("stat").is_none());
    assert_eq!(h.marks.marks().await.expect("marks"), Vec::new());
}

#[tokio::test]
async fn a_reference_reappearing_during_the_window_cancels_the_mark() {
    let h = Harness::new();
    let shared = h.store(b"an in-flight finalization's bytes").await;
    collect(&h.context, Mode::Apply).await.expect("a pass");
    assert_eq!(h.marks.marks().await.expect("marks").len(), 1);

    // The retry lands: an asset now references it.
    h.publish("late", b"an unrelated derivative").await;
    h.record(&AssetId::new("late"), BlobRole::Derivative, &shared)
        .await;

    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert_eq!(
        report.unmarked,
        vec![shared.clone()],
        "a reference reappearing mid-window must cancel the mark, not merely defer the sweep"
    );

    // And the window does not resume where it left off: past the original grace, still there.
    h.clock.advance(days(2));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(report.swept.is_empty());
    assert!(h.blobs.stat(&shared).await.expect("stat").is_some());
}

#[tokio::test]
async fn a_dry_run_changes_nothing() {
    let h = Harness::new();
    let orphan = h.store(b"an orphan under a dry run").await;

    let report = collect(&h.context, Mode::DryRun).await.expect("a pass");
    assert_eq!(report.marked, vec![orphan.clone()]);
    assert_eq!(
        h.marks.marks().await.expect("marks"),
        Vec::new(),
        "a dry run that marked would make the next real pass sweep a window early"
    );

    h.clock.advance(days(2));
    let report = collect(&h.context, Mode::DryRun).await.expect("a pass");
    assert_eq!(
        report.marked,
        vec![orphan.clone()],
        "still unmarked, so a second dry run reports the same first step"
    );
    assert!(h.blobs.stat(&orphan).await.expect("stat").is_some());
}

#[tokio::test]
async fn a_dangling_reference_is_reported_and_never_deleted() {
    let h = Harness::new();
    let held = h.publish("dangling", b"bytes that go missing").await;
    h.blobs.remove(&held).await.expect("the store removes");

    let found = dangling(&h.context, &[held.clone()])
        .await
        .expect("the check runs");
    assert_eq!(found, vec![held.clone()]);

    // And it is not collectable: the row still references it, so a collection pass sees a
    // reference count above zero for an address the store does not hold.
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(!report.swept.contains(&held));
    assert_eq!(
        h.index
            .read(&AssetId::new("dangling"))
            .await
            .expect("read")
            .expect("the row exists")
            .blobs
            .len(),
        3,
        "erasing the row would destroy the only record that the asset should exist"
    );
}

#[tokio::test]
async fn a_tombstoned_assets_blobs_are_still_referenced() {
    let h = Harness::new();
    let held = h.publish("deleted", b"trash still occupies storage").await;
    h.delete("deleted", Some(Timestamp::UNIX_EPOCH + days(30)), 1)
        .await;

    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(
        !report.marked.contains(&held),
        "deleting is not purging: the bytes stay until the signed retention window has passed, \
         which is what makes trash recoverable"
    );
}

#[tokio::test]
async fn the_purge_honours_the_signed_retention_floor() {
    let h = Harness::new();
    let held = h.publish("retained", b"a photo in the trash").await;
    h.delete("retained", Some(Timestamp::UNIX_EPOCH + days(30)), 1)
        .await;

    // Halfway through: refused, and reported as waiting.
    h.clock.advance(days(15));
    let report = purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert!(report.purged.is_empty());
    assert_eq!(report.retained, vec![AssetId::new("retained")]);

    // Past it: purged, and the blob becomes collectable.
    h.clock.advance(days(16));
    let report = purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert_eq!(report.purged, vec![AssetId::new("retained")]);

    let row = h
        .index
        .read(&AssetId::new("retained"))
        .await
        .expect("read")
        .expect("the row exists");
    assert!(row.blobs.is_empty());
    assert_eq!(
        row.state,
        crate::index::AssetState::Tombstoned,
        "the tombstone stays: a client that has not synced since the delete still has to learn \
         about it, so removing the row would make the deletion invisible rather than final"
    );

    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(report.marked.contains(&held));
}

#[tokio::test]
async fn a_tombstone_with_no_signed_retention_is_never_purged() {
    let h = Harness::new();
    h.publish("floorless", b"a delete whose manifest carried no floor")
        .await;
    h.delete("floorless", None, 1).await;

    h.clock.advance(days(3650));
    let report = purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert!(
        report.purged.is_empty(),
        "absent is not 'immediately' — reading it that way would purge exactly the assets whose \
         delete manifest the server failed to project a field out of"
    );
    assert_eq!(report.retained, vec![AssetId::new("floorless")]);
}

#[tokio::test]
async fn a_restore_clears_the_retention_floor() {
    let h = Harness::new();
    h.publish("restored", b"deleted then recovered").await;
    h.delete("restored", Some(Timestamp::UNIX_EPOCH + days(30)), 1)
        .await;

    let id = AssetId::new("restored");
    let head = h
        .index
        .read(&id)
        .await
        .expect("read")
        .expect("row")
        .chain_head;
    h.index
        .apply_op(LifecycleOp {
            asset_id: id.clone(),
            owner_id: OwnerId::new("gc-owner"),
            album_id: AlbumId::new("gc-album"),
            action: OpAction::TrashRestore,
            manifest_hash: Hash32([2; 32]),
            prior_provenance_hash: head,
            amk_version: 1,
            provenance: h.store(b"restored-manifest").await,
            original: None,
            metadata: None,
            retention_until: None,
            at: h.clock.now(),
        })
        .await
        .expect("the index applies");

    assert_eq!(
        h.index
            .read(&id)
            .await
            .expect("read")
            .expect("row")
            .retention_until,
        None,
        "an asset back in the live set has no window left to run out"
    );

    h.clock.advance(days(3650));
    let report = purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert!(
        report.purged.is_empty(),
        "a restored asset is not in the trash"
    );
    assert!(
        report.retained.is_empty(),
        "and it is not a tombstone at all"
    );
}

#[tokio::test]
async fn a_purge_dry_run_changes_nothing() {
    let h = Harness::new();
    h.publish("preview", b"a purge preview").await;
    h.delete("preview", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.clock.advance(days(1));

    let report = purge_expired(&h.context, Mode::DryRun, 10)
        .await
        .expect("a pass");
    assert_eq!(report.purged, vec![AssetId::new("preview")]);
    assert_eq!(
        h.index
            .read(&AssetId::new("preview"))
            .await
            .expect("read")
            .expect("row")
            .blobs
            .len(),
        3,
        "an operator who cannot preview a destructive batch will eventually run one blind"
    );
}

#[test]
fn the_system_clock_is_a_clock() {
    // Guards the seam the workers take: a production pass reads the trusted server clock, and
    // the retention floor is meaningless compared against anything else.
    let _: &dyn crate::store::Clock = &SystemClock;
}

#[tokio::test]
async fn a_swept_blob_credits_its_bytes_back_to_the_account_they_were_charged_to() {
    // `S-C44`. Without this an account's usage only ever goes up: emptying the trash frees disk
    // and frees nothing the user can see, and after enough cycles a quota reflects storage the
    // server no longer holds.
    let h = Harness::new();
    let address = h.publish("credited", b"derivative bytes").await;
    h.charge("uploader", &address, 16).await;
    assert_eq!(h.used("uploader").await, 16);

    h.delete("credited", Some(Timestamp::UNIX_EPOCH), 1).await;
    purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("the purge runs");

    // Marked, then swept once the grace window has passed.
    collect(&h.context, Mode::Apply)
        .await
        .expect("the collector marks");
    assert_eq!(
        h.used("uploader").await,
        16,
        "a mark is not a sweep: the bytes are still on disk and still charged"
    );

    h.clock.advance(days(31));
    let report = collect(&h.context, Mode::Apply)
        .await
        .expect("the collector sweeps");

    assert!(report.swept.contains(&address));
    assert!(
        report
            .credited
            .contains(&(crate::store::UserId::new("uploader"), 16)),
        "the pass must be able to say what it gave back and to whom: {:?}",
        report.credited
    );
    assert_eq!(h.used("uploader").await, 0);
}

#[tokio::test]
async fn a_blob_a_second_asset_still_references_credits_nothing() {
    // The reason the credit belongs to the sweep and not to the purge. Two assets share a
    // derivative, one is deleted and purged — refunding there would give back bytes the server
    // is still storing for the surviving holder.
    let h = Harness::new();
    let shared = h.publish("shared-first", b"shared derivative").await;
    let second = h.publish("shared-second", b"second asset").await;
    h.record(
        &AssetId::new("shared-second"),
        BlobRole::Derivative,
        &shared,
    )
    .await;
    h.charge("uploader", &shared, 16).await;

    h.delete("shared-first", Some(Timestamp::UNIX_EPOCH), 2)
        .await;
    purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("the purge runs");

    h.clock.advance(days(31));
    let report = collect(&h.context, Mode::Apply)
        .await
        .expect("the collector runs");

    assert!(!report.swept.contains(&shared), "it is still referenced");
    assert!(report.credited.is_empty());
    assert_eq!(
        h.used("uploader").await,
        16,
        "the surviving asset still holds these bytes, so the account still owes them"
    );
    // The second asset's own blobs are untouched, which is what makes the assertion above about
    // sharing rather than about nothing having happened.
    assert!(!report.swept.contains(&second));
}

#[tokio::test]
async fn a_swept_blob_the_ledger_never_saw_credits_nothing_and_is_not_an_error() {
    // The ordinary case for a blob that predates attribution, or one the ledger simply does not
    // hold. `release_attribution` answers `None`, and a sweep that treated that as a failure
    // would stall on the first such blob.
    let h = Harness::new();
    let address = h.publish("unattributed", b"never charged").await;
    h.delete("unattributed", Some(Timestamp::UNIX_EPOCH), 3)
        .await;
    purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("the purge runs");
    collect(&h.context, Mode::Apply)
        .await
        .expect("the collector marks");

    h.clock.advance(days(31));
    let report = collect(&h.context, Mode::Apply)
        .await
        .expect("the collector sweeps");

    assert!(report.swept.contains(&address));
    assert!(report.credited.is_empty());
}

#[tokio::test]
async fn a_dry_run_credits_nothing() {
    // A dry run must be readable without being a transaction. It reports what a real pass would
    // sweep and moves no bytes and no ledger entry.
    let h = Harness::new();
    let address = h.publish("dry", b"dry-run bytes").await;
    h.charge("uploader", &address, 16).await;
    h.delete("dry", Some(Timestamp::UNIX_EPOCH), 4).await;
    purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("the purge runs");
    collect(&h.context, Mode::Apply)
        .await
        .expect("the collector marks");
    h.clock.advance(days(31));

    let report = collect(&h.context, Mode::DryRun)
        .await
        .expect("the dry run runs");
    assert!(report.swept.contains(&address));
    assert!(report.credited.is_empty());
    assert_eq!(h.used("uploader").await, 16);
}
