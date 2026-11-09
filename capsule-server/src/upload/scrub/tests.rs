//! The startup scrub over the in-memory doubles: what it abandons, what it discards, and the
//! sessions it must leave alone.

use std::sync::Arc;

use capsule_core::crypto::hash::hash_bytes;
use uuid::Uuid;

use super::*;
use crate::blob::{BlobStore as _, ContentAddress, InMemoryBlobStore};
use crate::drop::{
    Admission, InMemoryDrops, LinkCaps, LinkProvisionOutcome, PendingDeposit, UploadLinkRecord,
};
use crate::index::memory::InMemoryAssetIndex;
use crate::quota::{ChargeOutcome, InMemoryQuota, QuotaContext, QuotaLimits, QuotaStore as _};
use crate::store::memory::{InMemoryUploadSessions, ManualClock};
use crate::store::{
    AlbumId, AssetId, BlobRole, FinalizeClaim, OwnerId, UploadSessionRecord, UploadSessionStatus,
    UserId,
};
use crate::upload::{
    AlbumWriteAccess, AuthorityError, AuthorityFuture, UploadPolicy, WriteAuthority,
};

/// An authority nothing here asks.
#[derive(Debug)]
struct Unasked;

impl WriteAuthority for Unasked {
    fn album_write_access<'a>(
        &'a self,
        _caller: &'a UserId,
        _album: &'a AlbumId,
    ) -> AuthorityFuture<'a, AlbumWriteAccess> {
        Box::pin(async { Err(AuthorityError::unavailable("not asked")) })
    }

    fn device_added_at<'a>(
        &'a self,
        _user: &'a UserId,
        _device: Uuid,
    ) -> AuthorityFuture<'a, Option<Timestamp>> {
        Box::pin(async { Err(AuthorityError::unavailable("not asked")) })
    }

    fn device_directory<'a>(
        &'a self,
        _user: &'a UserId,
    ) -> AuthorityFuture<'a, Option<capsule_core::crypto::keys::DeviceDirectory>> {
        Box::pin(async { Err(AuthorityError::unavailable("not asked")) })
    }
}

fn now() -> Timestamp {
    Timestamp::from_second(1_800_000_000).expect("in range")
}

fn uploader() -> UserId {
    UserId::new("scrub-uploader".to_owned())
}

/// The bytes session `tag` declares: distinct per session, so no two share a reservation.
fn bytes(tag: &str) -> Vec<u8> {
    format!("the bytes session {tag} declares").into_bytes()
}

fn address(tag: &str) -> ContentAddress {
    ContentAddress::parse(&hash_bytes(&bytes(tag)).to_hex()).expect("a content address")
}

fn id(tag: &str) -> UploadId {
    UploadId::new(format!("scrub-{tag}"))
}

/// A `Pending` session `tag` of the uploader's.
fn session(tag: &str) -> UploadSessionRecord {
    UploadSessionRecord {
        upload_id: id(tag),
        asset_id: AssetId::new(format!("scrub-asset-{tag}")),
        owner_id: OwnerId::new("scrub-uploader".to_owned()),
        upload_user_id: uploader(),
        album_id: None,
        content_type: Some("application/octet-stream".to_owned()),
        expected_hash: address(tag).as_str().to_owned(),
        crypto_suite_id: 1,
        protocol_version: "2026-08-29".to_owned(),
        blob_role: BlobRole::Original,
        intent_id: None,
        manifest_envelope: "{}".to_owned(),
        committed: crate::store::CommittedSet::default(),
        in_flight_cap: 1,
        total_size: bytes(tag).len() as u64,
        status: UploadSessionStatus::Pending,
        failure_code: None,
        created_at: now(),
        last_progress_at: now(),
    }
}

struct Harness {
    context: UploadContext,
    blobs: Arc<InMemoryBlobStore>,
    quotas: Arc<InMemoryQuota>,
    drops: InMemoryDrops,
}

fn harness() -> Harness {
    let clock = Arc::new(ManualClock::new(now()));
    let quotas = Arc::new(InMemoryQuota::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let context = UploadContext::new(
        Arc::new(InMemoryUploadSessions::with_default_ttl(clock.clone())),
        blobs.clone(),
        Arc::new(InMemoryAssetIndex::new()),
        Arc::new(Unasked),
        QuotaContext::new(quotas.clone(), clock.clone(), QuotaLimits::unlimited()),
        clock,
        UploadPolicy::default(),
    );
    Harness {
        context,
        blobs,
        quotas,
        drops: InMemoryDrops::new(),
    }
}

impl Harness {
    /// Open session `tag` in `status`, charged as creation charges it, with a stage or without.
    async fn open(&self, tag: &str, status: UploadSessionStatus, staged: bool) {
        let outcome = self
            .quotas
            .charge(
                &uploader(),
                &address(tag),
                bytes(tag).len() as u64,
                now(),
                QuotaLimits::unlimited(),
            )
            .await
            .expect("the ledger answers");
        assert!(matches!(outcome, ChargeOutcome::Charged { .. }));
        if staged {
            self.stage(tag).await;
        }
        self.context
            .sessions()
            .open(session(tag))
            .await
            .expect("opened");
        match status {
            UploadSessionStatus::Pending => {}
            UploadSessionStatus::WaitingForProcessing => {
                let claim = self
                    .context
                    .sessions()
                    .claim_finalize(&id(tag))
                    .await
                    .expect("the store answers");
                assert!(matches!(claim, FinalizeClaim::Won(_)), "{claim:?}");
            }
            status => {
                self.context
                    .sessions()
                    .set_status(&id(tag), status)
                    .await
                    .expect("the store answers")
                    .expect("the session is live");
            }
        }
    }

    /// Stage bytes for `tag`, as creation begins a stage and a chunk writes into it.
    async fn stage(&self, tag: &str) {
        self.blobs.begin(&id(tag)).await.expect("staged");
        self.blobs
            .write_staged(&id(tag), 0, &bytes(tag))
            .await
            .expect("written");
    }

    async fn is_staged(&self, tag: &str) -> bool {
        self.blobs
            .staged_len(&id(tag))
            .await
            .expect("the blob store answers")
            .is_some()
    }

    async fn status(&self, tag: &str) -> Option<UploadSessionStatus> {
        self.context
            .sessions()
            .read(&id(tag))
            .await
            .expect("the store answers")
            .map(|record| record.status)
    }

    async fn used(&self) -> u64 {
        self.quotas
            .usage(&uploader())
            .await
            .expect("the ledger answers")
            .used
    }
}

#[tokio::test]
async fn a_stage_no_session_holds_is_abandoned() {
    let h = harness();
    h.stage("orphan").await;

    let report = startup(&h.context, &h.drops).await;

    assert_eq!(report.abandoned, vec![id("orphan")]);
    assert!(report.discarded.is_empty(), "{report:?}");
    assert!(!h.is_staged("orphan").await, "the orphan's bytes are gone");
}

#[tokio::test]
async fn a_held_session_keeps_its_stage_whatever_its_status() {
    let h = harness();
    let statuses = [
        ("pending", UploadSessionStatus::Pending),
        ("uploading", UploadSessionStatus::Uploading),
        ("claimed", UploadSessionStatus::WaitingForProcessing),
        ("failed", UploadSessionStatus::FailedProcessing),
        ("completed", UploadSessionStatus::Completed),
    ];
    for (tag, status) in statuses {
        h.open(tag, status, true).await;
    }

    let report = startup(&h.context, &h.drops).await;

    assert_eq!(report, StartupScrub::default(), "nothing repaired");
    for (tag, status) in statuses {
        assert!(h.is_staged(tag).await, "{tag}'s stage is kept");
        assert_eq!(h.status(tag).await, Some(status), "{tag} is untouched");
    }
}

#[tokio::test]
async fn an_evictable_session_whose_stage_is_missing_is_discarded_and_its_reservation_released() {
    let h = harness();
    h.open("pending", UploadSessionStatus::Pending, false).await;
    h.open("uploading", UploadSessionStatus::Uploading, false)
        .await;
    assert!(h.used().await > 0, "both sessions are charged");

    let report = startup(&h.context, &h.drops).await;

    let mut discarded = report.discarded.clone();
    discarded.sort();
    assert_eq!(discarded, vec![id("pending"), id("uploading")]);
    assert!(report.abandoned.is_empty(), "{report:?}");
    assert_eq!(h.status("pending").await, None, "the uniform 404");
    assert_eq!(h.status("uploading").await, None, "the uniform 404");
    assert_eq!(h.used().await, 0, "both reservations are given back");
}

/// A one-file link whose only slot a guest's admission spent on session `tag`, held as a
/// pending deposit, as the drop create route leaves it. Returns the link's opaque id.
async fn spent_link(drops: &InMemoryDrops, tag: &str) -> String {
    let opaque_id = "0123456789abcdef0123456789abcdef".to_owned();
    let provisioned = drops
        .provision(UploadLinkRecord {
            opaque_id: opaque_id.clone(),
            owner_id: uploader(),
            drop_pubkey: vec![1; 32],
            crypto_suite_id: 1,
            protocol_version: "2026-08-29".to_owned(),
            caps: LinkCaps {
                max_file_count: Some(1),
                ..LinkCaps::default()
            },
            passphrase_verifier: None,
            used_bytes: 0,
            used_files: 0,
            revoked_at: None,
        })
        .await
        .expect("the drop store answers");
    assert!(matches!(provisioned, LinkProvisionOutcome::Provisioned));
    let size = bytes(tag).len() as u64;
    assert!(matches!(
        drops
            .charge(&opaque_id, size, now())
            .await
            .expect("answers"),
        Admission::Admitted { .. }
    ));
    drops
        .reserve(
            PendingDeposit {
                drop_id: format!("drop-{tag}"),
                opaque_id: opaque_id.clone(),
                owner_id: uploader(),
                kem_ct: vec![9; 64],
                content_type: "image/jpeg".to_owned(),
                suggested_filename: None,
                size,
            },
            &id(tag),
        )
        .await
        .expect("reserved");
    opaque_id
}

#[tokio::test]
async fn a_guest_drop_whose_stage_is_missing_gives_its_link_the_slot_back() {
    let h = harness();
    let outcome = h
        .quotas
        .charge(
            &uploader(),
            &address("drop"),
            bytes("drop").len() as u64,
            now(),
            QuotaLimits::unlimited(),
        )
        .await
        .expect("the ledger answers");
    assert!(matches!(outcome, ChargeOutcome::Charged { .. }));
    let mut record = session("drop");
    record.manifest_envelope = String::new();
    h.context.sessions().open(record).await.expect("opened");
    let link = spent_link(&h.drops, "drop").await;
    assert_eq!(
        h.drops.charge(&link, 1, now()).await.expect("answers"),
        Admission::CapExhausted,
        "the link's one slot is spent"
    );

    let report = startup(&h.context, &h.drops).await;

    assert_eq!(report.discarded, vec![id("drop")]);
    assert_eq!(h.status("drop").await, None, "the uniform 404");
    assert_eq!(h.used().await, 0, "the owner's quota is given back");
    assert_eq!(
        h.drops
            .take_reservation(&id("drop"))
            .await
            .expect("answers"),
        None,
        "the held declaration is taken"
    );
    assert!(
        matches!(
            h.drops.charge(&link, 1, now()).await.expect("answers"),
            Admission::Admitted { .. }
        ),
        "the link's one slot is usable again"
    );
}

#[tokio::test]
async fn a_claimed_or_terminal_session_without_a_stage_is_left_to_finalization_and_its_cap() {
    let h = harness();
    h.open("claimed", UploadSessionStatus::WaitingForProcessing, false)
        .await;
    h.open("failed", UploadSessionStatus::FailedProcessing, false)
        .await;
    h.open("completed", UploadSessionStatus::Completed, false)
        .await;

    let report = startup(&h.context, &h.drops).await;

    assert_eq!(report, StartupScrub::default(), "nothing repaired");
    assert_eq!(
        h.status("claimed").await,
        Some(UploadSessionStatus::WaitingForProcessing)
    );
    assert_eq!(
        h.status("failed").await,
        Some(UploadSessionStatus::FailedProcessing)
    );
    assert_eq!(
        h.status("completed").await,
        Some(UploadSessionStatus::Completed)
    );
}

#[tokio::test]
async fn both_directions_in_one_pass_and_a_second_boot_finds_nothing() {
    let h = harness();
    h.stage("orphan").await;
    h.open("unstaged", UploadSessionStatus::Uploading, false)
        .await;
    h.open("live", UploadSessionStatus::Uploading, true).await;

    let report = startup(&h.context, &h.drops).await;

    assert_eq!(report.abandoned, vec![id("orphan")]);
    assert_eq!(report.discarded, vec![id("unstaged")]);
    assert!(
        h.is_staged("live").await,
        "the live transfer keeps its stage"
    );
    assert_eq!(
        h.status("live").await,
        Some(UploadSessionStatus::Uploading),
        "the live transfer is untouched"
    );
    assert_eq!(startup(&h.context, &h.drops).await, StartupScrub::default());
}
