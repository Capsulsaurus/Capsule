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
use crate::attestation::{
    AttestationContext, InMemoryReceipts, LocalAttestationKey, PublishedKey, ReceiptSigner as _,
};
use crate::blob::{BlobStore, InMemoryBlobStore};
use crate::index::memory::InMemoryAssetIndex;
use crate::index::{
    AssetIndex, BlobRecord, LifecycleOp, OpAction, OpOutcome, PendingAsset, Reservation,
};
use crate::ledger::replay::LedgerKeys;
use crate::ledger::{InMemoryLedger, LedgerError, LedgerEvent, OwnerLedger};
use crate::store::{AlbumId, BlobRole, Clock, OwnerId, SystemClock};

/// The attestation key every harness signs its owner-ledger entries with.
fn attestation_key() -> Arc<LocalAttestationKey> {
    Arc::new(LocalAttestationKey::new(
        "gc.test",
        capsule_core::crypto::keys::HybridSigningKey::from_seed64(&[7; 64]),
    ))
}

/// An attestation context appending to `ledger`.
fn attestation_over(ledger: Arc<dyn OwnerLedger>) -> AttestationContext {
    AttestationContext::new(
        Arc::new(InMemoryReceipts::new()),
        attestation_key(),
        Timestamp::UNIX_EPOCH,
    )
    .with_ledger(ledger)
}

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
    shares: Arc<crate::share::InMemoryShares>,
    clock: Arc<StepClock>,
    /// The owner ledgers the purge appends to.
    ledger: Arc<InMemoryLedger>,
    /// Signs into [`Self::ledger`].
    attestation: AttestationContext,
}

impl Harness {
    fn new() -> Self {
        let index = Arc::new(InMemoryAssetIndex::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let marks = Arc::new(InMemoryCollection::new());
        let quotas = Arc::new(crate::quota::InMemoryQuota::new());
        let shares = Arc::new(crate::share::InMemoryShares::new());
        let clock = StepClock::new();
        let ledger = Arc::new(InMemoryLedger::new());
        let attestation = attestation_over(ledger.clone());
        let context = CollectionContext::new(
            index.clone(),
            blobs.clone(),
            marks.clone(),
            quotas.clone(),
            shares.clone(),
            clock.clone(),
            DEFAULT_GRACE_WINDOW,
        )
        .with_attestation(attestation.clone());
        Self {
            context,
            index,
            blobs,
            marks,
            quotas,
            shares,
            clock,
            ledger,
            attestation,
        }
    }

    /// A collector over these stores whose index is `index`, ledgering through `attestation`.
    fn collector(
        &self,
        index: Arc<dyn AssetIndex>,
        attestation: Option<AttestationContext>,
    ) -> CollectionContext {
        let context = CollectionContext::new(
            index,
            self.blobs.clone(),
            self.marks.clone(),
            self.quotas.clone(),
            self.shares.clone(),
            self.clock.clone(),
            DEFAULT_GRACE_WINDOW,
        );
        match attestation {
            Some(attestation) => context.with_attestation(attestation),
            None => context,
        }
    }

    /// The owner ledger of the harness's one owner, as appended so far.
    async fn ledger_entries(&self) -> Vec<crate::ledger::LedgerEntry> {
        self.ledger
            .scan(&OwnerId::new("gc-owner"))
            .await
            .expect("the ledger scans")
            .entries
    }

    /// Issue a link that serves `serves`, the first of them its metadata record.
    async fn share(
        &self,
        tag: u8,
        serves: &[&ContentAddress],
        expires_at: Option<Timestamp>,
    ) -> String {
        use crate::share::{IssueOutcome, ShareRecord, ShareStore as _};
        let opaque_id = format!("{tag:032x}");
        let outcome = self
            .shares
            .issue(ShareRecord {
                opaque_id: opaque_id.clone(),
                owner_id: crate::store::UserId::new("gc-owner"),
                scope: capsule_core::sharing::ShareScope::Album(uuid::Uuid::nil()),
                serves: serves.iter().map(|address| (*address).clone()).collect(),
                metadata: serves[0].clone(),
                wrapped_secret: None,
                expires_at,
                revoked_at: None,
            })
            .await
            .expect("the share store issues");
        assert_eq!(outcome, IssueOutcome::Issued);
        opaque_id
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
                    amk_version: None,
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

/// #653: a share link's sealed metadata record has no asset row, and a live link must keep it.
#[tokio::test]
async fn a_blob_a_live_share_serves_is_never_marked() {
    let h = Harness::new();
    let record = h.store(b"a sealed share-metadata/v1 record").await;
    let prepared = h.store(b"a blob prepared for the boundary crossing").await;
    h.share(1, &[&record, &prepared], None).await;

    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(report.marked.is_empty(), "{report:?}");
    h.clock.advance(days(3));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(
        report.marked.is_empty() && report.swept.is_empty(),
        "{report:?}"
    );
    assert!(h.blobs.stat(&record).await.expect("stat").is_some());
    assert!(h.blobs.stat(&prepared).await.expect("stat").is_some());
}

/// Once the last link naming a blob is revoked, the blob is an ordinary orphan.
#[tokio::test]
async fn a_revoked_shares_blob_is_marked_then_swept() {
    use crate::share::ShareStore as _;
    let h = Harness::new();
    let record = h.store(b"a record whose link is revoked").await;
    let id = h.share(2, &[&record], None).await;
    assert!(
        h.shares
            .revoke(&crate::store::UserId::new("gc-owner"), &id, h.clock.now())
            .await
            .expect("revoke")
    );

    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert_eq!(report.marked, vec![record.clone()]);
    h.clock.advance(days(2));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert_eq!(report.swept, vec![record.clone()]);
    assert!(h.blobs.stat(&record).await.expect("stat").is_none());
}

/// An expiring link holds its blob until the expiry and not after; a link issued mid-window over
/// the same blob cancels the mark, exactly as an asset reference reappearing would.
#[tokio::test]
async fn an_expired_shares_blob_is_collectable_and_a_new_link_reprieves_it() {
    let h = Harness::new();
    let record = h.store(b"a record whose link expires").await;
    h.share(3, &[&record], Some(h.clock.now() + days(1))).await;

    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(
        report.marked.is_empty(),
        "live until its expiry: {report:?}"
    );

    h.clock.advance(days(1));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert_eq!(
        report.marked,
        vec![record.clone()],
        "not live at its expiry instant"
    );

    h.share(4, &[&record], None).await;
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert_eq!(report.unmarked, vec![record.clone()]);
    h.clock.advance(days(2));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(report.swept.is_empty(), "{report:?}");
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

    let found = dangling(&h.context, std::slice::from_ref(&held))
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
async fn a_purged_tombstone_is_not_reported_again_nor_holds_a_bounded_pass_back() {
    let h = Harness::new();
    h.publish("first", b"deleted first").await;
    h.delete("first", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.clock.advance(days(1));
    let report = purge_expired(&h.context, Mode::Apply, 1)
        .await
        .expect("a pass");
    assert_eq!(report.purged, vec![AssetId::new("first")]);

    // A later deletion, behind the purged one in the oldest-first listing.
    h.publish("second", b"deleted later").await;
    h.delete("second", Some(Timestamp::UNIX_EPOCH), 2).await;
    h.clock.advance(days(1));
    let report = purge_expired(&h.context, Mode::Apply, 1)
        .await
        .expect("a pass");
    assert_eq!(
        report.purged,
        vec![AssetId::new("second")],
        "a page of one goes to the tombstone still holding bytes, not the one already emptied"
    );

    let report = purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert_eq!(report, PurgeReport::default(), "nothing is left to purge");
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
    // Without this an account's usage only ever goes up: emptying the trash frees disk
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

/// An index that changes a listed row after the purge worker has listed its tombstones and
/// before it purges them, made deterministic: it lands a `restore` (the interleaving a real
/// `TrashRestore` can produce), or treats the `dropped` row as gone by the time it is purged.
#[derive(Debug)]
struct RacesTheListing {
    inner: Arc<InMemoryAssetIndex>,
    restore: std::sync::Mutex<Option<LifecycleOp>>,
    dropped: Option<AssetId>,
    /// When set, every `purge` call first records how many entries this owner ledger holds, so
    /// a test can assert the purge entry was durable before the index was asked.
    ledger: Option<Arc<InMemoryLedger>>,
    /// The ledger lengths [`Self::ledger`] recorded, one per `purge` call.
    ledger_at_purge: std::sync::Mutex<Vec<usize>>,
}

impl RacesTheListing {
    /// An index that changes nothing between the listing and the purge.
    fn new(inner: Arc<InMemoryAssetIndex>) -> Self {
        Self {
            inner,
            restore: std::sync::Mutex::new(None),
            dropped: None,
            ledger: None,
            ledger_at_purge: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl AssetIndex for RacesTheListing {
    fn reserve(&self, asset: PendingAsset) -> crate::index::IndexFuture<'_, Reservation> {
        self.inner.reserve(asset)
    }

    fn read<'a>(
        &'a self,
        asset: &'a AssetId,
    ) -> crate::index::IndexFuture<'a, Option<crate::index::AssetRow>> {
        self.inner.read(asset)
    }

    fn record_blob<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
    ) -> crate::index::IndexFuture<'a, crate::index::BlobOutcome> {
        self.inner.record_blob(asset, blob)
    }

    fn record_blob_with_receipt<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
        custody: crate::index::Custody<'a>,
    ) -> crate::index::IndexFuture<'a, crate::index::Receipted<crate::index::BlobOutcome>> {
        self.inner.record_blob_with_receipt(asset, blob, custody)
    }

    fn tombstone<'a>(
        &'a self,
        asset: &'a AssetId,
        at: Timestamp,
    ) -> crate::index::IndexFuture<'a, Option<crate::index::AssetRow>> {
        self.inner.tombstone(asset, at)
    }

    fn find_by_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        address: &'a ContentAddress,
    ) -> crate::index::IndexFuture<'a, Option<AssetId>> {
        self.inner.find_by_address(owner, album, address)
    }

    fn find_references<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> crate::index::IndexFuture<'a, Vec<crate::index::BlobReference>> {
        self.inner.find_references(address)
    }

    fn apply_op(&self, op: LifecycleOp) -> crate::index::IndexFuture<'_, OpOutcome> {
        self.inner.apply_op(op)
    }

    fn apply_op_with_receipt<'a>(
        &'a self,
        op: LifecycleOp,
        custody: crate::index::Custody<'a>,
    ) -> crate::index::IndexFuture<'a, crate::index::Receipted<OpOutcome>> {
        self.inner.apply_op_with_receipt(op, custody)
    }

    fn set_hold<'a>(
        &'a self,
        asset: &'a AssetId,
        hold: Option<crate::index::ServingHold>,
    ) -> crate::index::IndexFuture<'a, crate::index::HoldOutcome> {
        self.inner.set_hold(asset, hold)
    }

    fn reference_count<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> crate::index::IndexFuture<'a, u64> {
        self.inner.reference_count(address)
    }

    fn rows<'a>(
        &'a self,
        after: Option<&'a AssetId>,
        limit: usize,
    ) -> crate::index::IndexFuture<'a, Vec<crate::index::AssetRow>> {
        self.inner.rows(after, limit)
    }

    fn tombstoned(
        &self,
        limit: usize,
    ) -> crate::index::IndexFuture<'_, Vec<crate::index::AssetRow>> {
        Box::pin(async move {
            let listed = self.inner.tombstoned(limit).await?;
            let restore = self
                .restore
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(restore) = restore {
                let outcome = self.inner.apply_op(restore).await?;
                assert!(matches!(outcome, OpOutcome::Applied { .. }), "{outcome:?}");
            }
            Ok(listed)
        })
    }

    fn purge<'a>(
        &'a self,
        asset: &'a AssetId,
        now: Timestamp,
    ) -> crate::index::IndexFuture<'a, crate::index::PurgeOutcome> {
        Box::pin(async move {
            if let Some(ledger) = &self.ledger {
                let held = ledger
                    .scan(&OwnerId::new("gc-owner"))
                    .await
                    .expect("the ledger scans")
                    .entries
                    .len();
                self.ledger_at_purge
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(held);
            }
            if self.dropped.as_ref() == Some(asset) {
                return Ok(crate::index::PurgeOutcome::Unknown);
            }
            self.inner.purge(asset, now).await
        })
    }

    fn feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        after: u64,
        limit: usize,
    ) -> crate::index::IndexFuture<'a, Vec<crate::index::FeedEntry>> {
        self.inner.feed_page(owner, after, limit)
    }

    fn head_seq<'a>(&'a self, owner: &'a OwnerId) -> crate::index::IndexFuture<'a, u64> {
        self.inner.head_seq(owner)
    }

    fn album_feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        after: u64,
        limit: usize,
    ) -> crate::index::IndexFuture<'a, Vec<crate::index::FeedEntry>> {
        self.inner.album_feed_page(owner, album, after, limit)
    }

    fn album_head_seq<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
    ) -> crate::index::IndexFuture<'a, u64> {
        self.inner.album_head_seq(owner, album)
    }

    fn sync_epoch<'a>(&'a self, owner: &'a OwnerId) -> crate::index::IndexFuture<'a, u64> {
        self.inner.sync_epoch(owner)
    }

    fn bump_sync_epoch<'a>(&'a self, owner: &'a OwnerId) -> crate::index::IndexFuture<'a, u64> {
        self.inner.bump_sync_epoch(owner)
    }

    fn ledger_head<'a>(
        &'a self,
        owner: &'a OwnerId,
    ) -> crate::index::IndexFuture<'a, Option<crate::ledger::LedgerHead>> {
        self.inner.ledger_head(owner)
    }

    fn anchored_owners(&self) -> crate::index::IndexFuture<'_, Vec<OwnerId>> {
        self.inner.anchored_owners()
    }

    fn record_ledger_head<'a>(
        &'a self,
        owner: &'a OwnerId,
        head: crate::ledger::LedgerHead,
    ) -> crate::index::IndexFuture<'a, ()> {
        self.inner.record_ledger_head(owner, head)
    }

    fn reset_ledger_head<'a>(
        &'a self,
        owner: &'a OwnerId,
        head: Option<crate::ledger::LedgerHead>,
    ) -> crate::index::IndexFuture<'a, ()> {
        self.inner.reset_ledger_head(owner, head)
    }

    fn restore(
        &self,
        row: crate::index::AssetRow,
    ) -> crate::index::IndexFuture<'_, crate::index::RestoreOutcome> {
        self.inner.restore(row)
    }
}

#[tokio::test]
async fn a_restore_between_listing_and_purge_keeps_its_bytes() {
    let h = Harness::new();
    let held = h.publish("raced", b"restored while the purge ran").await;
    // The window closes an hour from now. The restore below is accepted (its `at` stamped)
    // inside it, and reaches the index only after a purge a day later has listed the row: the
    // one restore that can race a purge, since one accepted after the floor is refused.
    h.delete(
        "raced",
        Some(h.clock.now() + SignedDuration::from_hours(1)),
        1,
    )
    .await;

    let id = AssetId::new("raced");
    let before = h.index.read(&id).await.expect("read").expect("row");
    let restore = LifecycleOp {
        asset_id: id.clone(),
        owner_id: OwnerId::new("gc-owner"),
        album_id: AlbumId::new("gc-album"),
        action: OpAction::TrashRestore,
        manifest_hash: Hash32([2; 32]),
        prior_provenance_hash: before.chain_head,
        amk_version: 1,
        provenance: h.store(b"raced-restore-manifest").await,
        original: None,
        metadata: None,
        retention_until: None,
        at: h.clock.now(),
    };
    h.clock.advance(days(1));
    let racing = Arc::new(RacesTheListing::new(h.index.clone()));
    *racing
        .restore
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(restore);
    let context = h.collector(racing, Some(h.attestation.clone()));

    let report = purge_expired(&context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert!(
        report.purged.is_empty(),
        "the listing saw a tombstone, but the row was restored before the purge took its lock"
    );
    assert_eq!(
        report.skipped,
        vec![id.clone()],
        "the report accounts for the listed row the re-check kept"
    );

    let row = h.index.read(&id).await.expect("read").expect("row");
    assert_eq!(row.state, crate::index::AssetState::Visible);
    assert!(
        row.blobs.iter().any(|blob| blob.address == held),
        "a restored asset keeps its references"
    );

    // Two collection passes a grace window apart: a cleared reference would be marked by the
    // first and swept by the second.
    collect(&h.context, Mode::Apply).await.expect("a pass");
    h.clock.advance(days(2));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(!report.swept.contains(&held), "{report:?}");
    assert!(
        h.blobs.stat(&held).await.expect("stat").is_some(),
        "the restored asset's bytes are still on disk"
    );
}

#[tokio::test]
async fn a_restore_after_the_window_is_refused_and_the_purge_proceeds() {
    let h = Harness::new();
    let held = h.publish("late", b"restored too late").await;
    h.delete(
        "late",
        Some(h.clock.now() + SignedDuration::from_hours(1)),
        1,
    )
    .await;
    h.clock.advance(days(1));

    let id = AssetId::new("late");
    let before = h.index.read(&id).await.expect("read").expect("row");
    let outcome = h
        .index
        .apply_op(LifecycleOp {
            asset_id: id.clone(),
            owner_id: OwnerId::new("gc-owner"),
            album_id: AlbumId::new("gc-album"),
            action: OpAction::TrashRestore,
            manifest_hash: Hash32([2; 32]),
            prior_provenance_hash: before.chain_head,
            amk_version: 1,
            provenance: h.store(b"late-restore-manifest").await,
            original: None,
            metadata: None,
            retention_until: None,
            at: h.clock.now(),
        })
        .await
        .expect("the index answers");
    assert!(
        matches!(outcome, OpOutcome::RetentionElapsed { .. }),
        "a restore the server accepts after the floor is refused: {outcome:?}"
    );
    assert_eq!(
        h.index.read(&id).await.expect("read").expect("row"),
        before,
        "the refusal wrote nothing"
    );

    let report = purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert_eq!(
        report.purged,
        vec![id.clone()],
        "the refused restore left the tombstone to its purge"
    );
    let row = h.index.read(&id).await.expect("read").expect("row");
    assert_eq!(row.state, crate::index::AssetState::Tombstoned);
    assert!(!row.blobs.iter().any(|blob| blob.address == held));
}

#[tokio::test]
async fn a_listed_tombstone_whose_row_is_gone_at_purge_time_is_skipped() {
    let h = Harness::new();
    h.publish("vanished", b"its row went before the purge")
        .await;
    h.delete("vanished", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.publish("expired", b"purged in the same pass").await;
    h.delete("expired", Some(Timestamp::UNIX_EPOCH), 2).await;
    h.clock.advance(days(1));

    let id = AssetId::new("vanished");
    let racing = Arc::new(RacesTheListing {
        dropped: Some(id.clone()),
        ..RacesTheListing::new(h.index.clone())
    });
    let context = h.collector(racing, Some(h.attestation.clone()));

    let report = purge_expired(&context, Mode::Apply, 10)
        .await
        .expect("a missing row is accounted for, not an error that fails the pass");
    assert_eq!(
        report.skipped,
        vec![id],
        "the listed row the purge found gone is reported as skipped"
    );
    assert_eq!(
        report.purged,
        vec![AssetId::new("expired")],
        "the rest of the page is still purged"
    );
    assert!(report.retained.is_empty(), "{report:?}");
}

/// A [`ProgressSweep`] that records the instant of every sweep, or fails when told to.
#[derive(Debug, Default)]
struct RecordingSweep {
    swept_at: std::sync::Mutex<Vec<Timestamp>>,
    fails: bool,
}

impl ProgressSweep for RecordingSweep {
    fn sweep_progress(&self, now: Timestamp) -> StoreFuture<'_, usize> {
        Box::pin(async move {
            if self.fails {
                return Err(StoreError::Unavailable {
                    store: "uploads",
                    detail: "the session store is down".to_owned(),
                });
            }
            self.swept_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(now);
            Ok(2)
        })
    }
}

impl RecordingSweep {
    fn sweeps(&self) -> Vec<Timestamp> {
        self.swept_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[tokio::test]
async fn an_applied_pass_sweeps_the_upload_progress_view_at_its_own_instant() {
    let h = Harness::new();
    let sweep = Arc::new(RecordingSweep::default());
    let context = h.context.clone().with_progress_sweep(sweep.clone());
    h.clock.advance(days(3));

    collect(&context, Mode::Apply).await.expect("a pass");

    assert_eq!(
        sweep.sweeps(),
        vec![h.clock.now()],
        "one sweep per applied pass, as of the pass's clock"
    );
}

#[tokio::test]
async fn a_dry_run_leaves_the_upload_progress_view_unswept() {
    let h = Harness::new();
    let sweep = Arc::new(RecordingSweep::default());
    let context = h.context.clone().with_progress_sweep(sweep.clone());

    collect(&context, Mode::DryRun).await.expect("a pass");

    assert!(
        sweep.sweeps().is_empty(),
        "the sweep removes entries, and a dry run removes nothing"
    );
}

#[tokio::test]
async fn a_failed_sweep_fails_the_pass_after_the_blobs_were_decided() {
    let h = Harness::new();
    let orphan = h.store(b"an orphan the pass still marks").await;
    let sweep = Arc::new(RecordingSweep {
        fails: true,
        ..RecordingSweep::default()
    });
    let context = h.context.clone().with_progress_sweep(sweep);

    let error = collect(&context, Mode::Apply)
        .await
        .expect_err("a session store that cannot answer fails the pass");

    assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
    assert!(
        h.marks
            .marked_since(&orphan)
            .await
            .expect("a lookup")
            .is_some(),
        "the collection ran to the end before the sweep was tried"
    );
}

#[tokio::test]
async fn the_upload_session_store_sweeps_through_its_pressure_listing() {
    // The production implementation of the port: an unbounded limit at a horizon of `now` must
    // reach every evictable session, which the in-memory double answers in full.
    let clock = StepClock::new();
    let uploads: Arc<dyn crate::store::UploadSessionStore> =
        Arc::new(crate::store::memory::InMemoryUploadSessions::with_default_ttl(clock.clone()));
    for n in 0..3 {
        uploads
            .open(crate::store::upload::UploadSessionRecord {
                upload_id: crate::store::UploadId::new(format!("gc-sweep-{n}")),
                asset_id: AssetId::new(format!("gc-sweep-asset-{n}")),
                owner_id: OwnerId::new("gc-owner"),
                upload_user_id: crate::store::UserId::new("gc-uploader"),
                album_id: None,
                content_type: None,
                expected_hash: format!("{n:064x}"),
                crypto_suite_id: 1,
                protocol_version: "2026-08-29".to_owned(),
                blob_role: BlobRole::Original,
                intent_id: None,
                manifest_envelope: "{}".to_owned(),
                received_bytes: 0,
                total_size: 1,
                status: crate::store::upload::UploadSessionStatus::Pending,
                created_at: clock.now(),
                last_progress_at: clock.now(),
            })
            .await
            .expect("the session opens");
    }
    clock.advance(SignedDuration::from_secs(1));

    let evictable = uploads.sweep_progress(clock.now()).await.expect("a sweep");

    assert_eq!(evictable, 3);
}

/// An owner ledger that refuses every append, as a full or unwritable blob root would.
#[derive(Debug)]
struct RefusingLedger;

impl OwnerLedger for RefusingLedger {
    fn append<'a>(
        &'a self,
        _owner: &'a OwnerId,
        _event: LedgerEvent,
        _recorded_at: Timestamp,
        _signer: &'a dyn crate::attestation::ReceiptSigner,
    ) -> crate::ledger::LedgerFuture<'a, crate::ledger::LedgerEntry> {
        Box::pin(async {
            Err(LedgerError::StorageFull {
                operation: "append",
            })
        })
    }

    fn scan<'a>(
        &'a self,
        _owner: &'a OwnerId,
    ) -> crate::ledger::LedgerFuture<'a, crate::ledger::LedgerScan> {
        Box::pin(async {
            Ok(crate::ledger::LedgerScan {
                entries: Vec::new(),
                damage: None,
            })
        })
    }

    fn owners(&self) -> crate::ledger::LedgerFuture<'_, Vec<OwnerId>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Refused like an append: a full ledger keeps nothing beside it either.
    fn attach<'a>(
        &'a self,
        _owner: &'a OwnerId,
        _key: capsule_core::crypto::hash::Hash32,
        _bytes: Vec<u8>,
    ) -> crate::ledger::LedgerFuture<'a, ()> {
        Box::pin(async {
            Err(LedgerError::StorageFull {
                operation: "attach",
            })
        })
    }

    fn attachment<'a>(
        &'a self,
        _owner: &'a OwnerId,
        _key: &'a capsule_core::crypto::hash::Hash32,
    ) -> crate::ledger::LedgerFuture<'a, Option<Vec<u8>>> {
        Box::pin(async { Ok(None) })
    }
}

/// The purge entry `purge_expired` writes for `asset` before the index call.
fn purge_event(asset: &str) -> LedgerEvent {
    purge_entry(asset, PURGE_ACTION, None)
}

/// The entry `purge_expired` writes for `asset` once the index applied the purge ledgered at
/// `purge_seq` (#811).
fn purged_event(asset: &str, purge_seq: u64) -> LedgerEvent {
    purge_entry(asset, PURGED_ACTION, Some(purge_seq))
}

fn purge_entry(asset: &str, action: &str, sync_seq: Option<u64>) -> LedgerEvent {
    LedgerEvent::Lifecycle {
        asset_id: asset.to_owned(),
        action: action.to_owned(),
        manifest_hash: None,
        sync_seq,
    }
}

#[tokio::test]
async fn a_purge_appends_its_ledger_entry_before_the_index_write() {
    // `design/filesystem/server.md`: an entry is appended **before** the index row commits, so a
    // crash between the two leaves an entry without a row change, never the reverse.
    let h = Harness::new();
    h.publish("ledgered", b"purged under a signed floor").await;
    h.delete("ledgered", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.clock.advance(days(1));

    let observed = Arc::new(RacesTheListing {
        ledger: Some(h.ledger.clone()),
        ..RacesTheListing::new(h.index.clone())
    });
    let context = h.collector(observed.clone(), Some(h.attestation.clone()));
    let report = purge_expired(&context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert_eq!(report.purged, vec![AssetId::new("ledgered")]);

    let entries = h.ledger_entries().await;
    assert_eq!(
        entries.len(),
        2,
        "one purge: its entry and its confirmation"
    );
    assert_eq!(entries[0].core.event, purge_event("ledgered"));
    assert_eq!(
        entries[1].core.event,
        purged_event("ledgered", 1),
        "the confirmation follows the index's answer"
    );
    assert_eq!(entries[0].core.recorded_at, h.clock.now().to_string());
    assert_eq!(
        *observed
            .ledger_at_purge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![1],
        "the purge entry was durable when the index was asked to purge"
    );
}

#[tokio::test]
async fn a_failed_ledger_append_leaves_the_tombstones_blobs_referenced() {
    let h = Harness::new();
    let held = h.publish("unledgered", b"the ledger is full").await;
    h.delete("unledgered", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.clock.advance(days(1));

    let refusing = h.collector(
        h.index.clone(),
        Some(attestation_over(Arc::new(RefusingLedger))),
    );
    let report = purge_expired(&refusing, Mode::Apply, 10)
        .await
        .expect("a failed append skips the asset rather than failing the pass");
    let id = AssetId::new("unledgered");
    assert!(report.purged.is_empty(), "{report:?}");
    assert_eq!(report.unrecorded, vec![id.clone()], "counted in the report");

    let row = h.index.read(&id).await.expect("read").expect("row");
    assert_eq!(row.state, crate::index::AssetState::Tombstoned);
    assert!(
        row.blobs.iter().any(|blob| blob.address == held),
        "a purge the ledger does not hold is not applied"
    );

    // Two collection passes a grace window apart: a dropped reference would be swept.
    collect(&h.context, Mode::Apply).await.expect("a pass");
    h.clock.advance(days(2));
    let report = collect(&h.context, Mode::Apply).await.expect("a pass");
    assert!(!report.swept.contains(&held), "{report:?}");

    // The next pass, with a ledger that answers, purges it.
    let report = purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert_eq!(report.purged, vec![id]);
    assert!(report.unrecorded.is_empty());
}

#[tokio::test]
async fn the_owners_chain_verifies_after_a_delete_followed_by_a_purge() {
    let h = Harness::new();
    h.publish("chained", b"deleted, then purged").await;
    // The delete's entry, appended as the lifecycle route appends it before the index call.
    h.attestation
        .append_to_ledger(
            &OwnerId::new("gc-owner"),
            LedgerEvent::Lifecycle {
                asset_id: "chained".to_owned(),
                action: OpAction::Delete.as_str().to_owned(),
                manifest_hash: Some(Hash32([1; 32])),
                sync_seq: None,
            },
            h.clock.now(),
        )
        .await
        .expect("the delete is ledgered");
    h.delete("chained", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.clock.advance(days(1));

    let report = purge_expired(&h.context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert_eq!(report.purged, vec![AssetId::new("chained")]);

    let owner = OwnerId::new("gc-owner");
    let scan = h.ledger.scan(&owner).await.expect("the ledger scans");
    let key = attestation_key();
    let keys = LedgerKeys::new(vec![PublishedKey {
        key_id: key.key_id(),
        public: key.verifying_key(),
        active_from: Timestamp::UNIX_EPOCH,
        active_to: None,
    }]);
    let chain = keys.verify(&owner, &scan, None);
    assert_eq!(chain.fault, None, "{chain:?}");
    assert_eq!(
        chain.verified, 3,
        "the delete, the purge and its confirmation all verify"
    );
    assert_eq!(scan.entries[1].core.event, purge_event("chained"));
    assert_eq!(scan.entries[2].core.event, purged_event("chained", 2));

    // Replay reads the purge as a lifecycle op of its own, confirmed, which rebuild empties the
    // tombstone for; the confirmation is not an op.
    let ops = crate::ledger::replay::lifecycle(&scan.entries);
    assert_eq!(
        ops.iter().map(|op| op.action.as_str()).collect::<Vec<_>>(),
        vec![OpAction::Delete.as_str(), PURGE_ACTION]
    );
    assert!(ops[1].confirmed, "{ops:?}");
}

#[tokio::test]
async fn a_purge_the_index_no_longer_allows_is_never_confirmed() {
    // #811: the purge entry is confirmed only once the index answers `Purged`. When the
    // re-check keeps the row, nothing follows it, and rebuild keeps the asset's references.
    let h = Harness::new();
    h.publish("vanished", b"its row went before the purge")
        .await;
    h.delete("vanished", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.clock.advance(days(1));

    let id = AssetId::new("vanished");
    let racing = Arc::new(RacesTheListing {
        dropped: Some(id.clone()),
        ..RacesTheListing::new(h.index.clone())
    });
    let context = h.collector(racing, Some(h.attestation.clone()));
    let report = purge_expired(&context, Mode::Apply, 10)
        .await
        .expect("a pass");
    assert_eq!(report.skipped, vec![id]);

    let entries = h.ledger_entries().await;
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.core.event.clone())
            .collect::<Vec<_>>(),
        vec![purge_event("vanished")]
    );
}

#[tokio::test]
async fn an_applied_purge_without_an_attestation_context_is_refused_before_it_lists() {
    let h = Harness::new();
    let held = h.publish("unsigned", b"no key on this host").await;
    h.delete("unsigned", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.clock.advance(days(1));

    let keyless = h.collector(h.index.clone(), None);
    let error = purge_expired(&keyless, Mode::Apply, 10)
        .await
        .expect_err("a purge no ledger can record is not applied");
    assert!(
        error.to_string().contains("ATTESTATION_KEY_SEED"),
        "{error}"
    );

    let row = h
        .index
        .read(&AssetId::new("unsigned"))
        .await
        .expect("read")
        .expect("row");
    assert!(row.blobs.iter().any(|blob| blob.address == held));

    // A dry run writes nothing, so it needs no key.
    let report = purge_expired(&keyless, Mode::DryRun, 10)
        .await
        .expect("a dry run");
    assert_eq!(report.purged, vec![AssetId::new("unsigned")]);
    assert!(h.ledger_entries().await.is_empty());
}

#[tokio::test]
async fn a_purge_dry_run_appends_nothing_to_the_ledger() {
    let h = Harness::new();
    h.publish("preview", b"a purge preview").await;
    h.delete("preview", Some(Timestamp::UNIX_EPOCH), 1).await;
    h.clock.advance(days(1));

    purge_expired(&h.context, Mode::DryRun, 10)
        .await
        .expect("a pass");
    assert!(h.ledger_entries().await.is_empty());
}
