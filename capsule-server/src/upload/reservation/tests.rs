//! When an ended session's reservation goes back, and the two holders that keep it.

use std::sync::Arc;

use capsule_core::crypto::hash::hash_bytes;
use jiff::Timestamp;
use uuid::Uuid;

use super::*;
use crate::blob::{BlobStore as _, InMemoryBlobStore};
use crate::index::memory::InMemoryAssetIndex;
use crate::quota::{ChargeOutcome, InMemoryQuota, QuotaContext, QuotaLimits, QuotaStore as _};
use crate::store::memory::{InMemoryUploadSessions, ManualClock};
use crate::store::{AlbumId, AssetId, BlobRole, OwnerId, UploadId, UploadSessionStatus, UserId};
use crate::upload::{
    AlbumWriteAccess, AuthorityError, AuthorityFuture, UploadPolicy, WriteAuthority,
};

/// The bytes every case reserves.
const BYTES: &[u8] = b"the bytes a session reserved";

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

/// A ledger whose release lets a create open its session first: the session `arrival` holds
/// becomes visible after the release's live-session check and before the ledger releases.
#[derive(Debug)]
struct RacingQuota {
    inner: Arc<InMemoryQuota>,
    sessions: Arc<InMemoryUploadSessions>,
    arrival: std::sync::Mutex<Option<UploadSessionRecord>>,
}

impl crate::quota::QuotaStore for RacingQuota {
    fn usage<'a>(
        &'a self,
        user: &'a UserId,
    ) -> crate::store::StoreFuture<'a, crate::quota::StoredUsage> {
        self.inner.usage(user)
    }

    fn charge<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        limits: QuotaLimits,
    ) -> crate::store::StoreFuture<'a, ChargeOutcome> {
        self.inner.charge(user, address, size, at, limits)
    }

    fn release<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
    ) -> crate::store::StoreFuture<'a, bool> {
        let arrival = self
            .arrival
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        Box::pin(async move {
            if let Some(arrival) = arrival {
                crate::store::UploadSessionStore::open(self.sessions.as_ref(), arrival).await?;
            }
            self.inner.release(user, address).await
        })
    }

    fn release_attribution<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> crate::store::StoreFuture<'a, Option<(UserId, u64)>> {
        self.inner.release_attribution(address)
    }

    fn suspend<'a>(
        &'a self,
        user: &'a UserId,
        at: Timestamp,
    ) -> crate::store::StoreFuture<'a, bool> {
        self.inner.suspend(user, at)
    }

    fn unsuspend<'a>(&'a self, user: &'a UserId) -> crate::store::StoreFuture<'a, bool> {
        self.inner.unsuspend(user)
    }
}

/// A harness whose ledger opens `arrival` in the middle of the next release.
fn racing_harness(arrival: UploadSessionRecord) -> Harness {
    let clock = Arc::new(ManualClock::new(
        Timestamp::from_second(1_800_000_000).expect("in range"),
    ));
    let quotas = Arc::new(InMemoryQuota::new());
    let sessions = Arc::new(InMemoryUploadSessions::with_default_ttl(clock.clone()));
    let blobs = Arc::new(InMemoryBlobStore::new());
    let racing = Arc::new(RacingQuota {
        inner: quotas.clone(),
        sessions: sessions.clone(),
        arrival: std::sync::Mutex::new(Some(arrival)),
    });
    let context = UploadContext::new(
        sessions,
        blobs.clone(),
        Arc::new(InMemoryAssetIndex::new()),
        Arc::new(Unasked),
        QuotaContext::new(racing, clock.clone(), QuotaLimits::unlimited()),
        clock,
        UploadPolicy::default(),
    );
    Harness {
        context,
        quotas,
        blobs,
    }
}

struct Harness {
    context: UploadContext,
    quotas: Arc<InMemoryQuota>,
    blobs: Arc<InMemoryBlobStore>,
}

fn harness() -> Harness {
    let clock = Arc::new(ManualClock::new(
        Timestamp::from_second(1_800_000_000).expect("in range"),
    ));
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
        quotas,
        blobs,
    }
}

fn uploader() -> UserId {
    UserId::new("reservation-uploader".to_owned())
}

fn hash() -> String {
    hash_bytes(BYTES).to_hex()
}

fn address() -> ContentAddress {
    ContentAddress::parse(&hash()).expect("a content address")
}

/// A session `tag` of the uploader's declaring [`BYTES`], in `status`.
fn session(tag: &str, status: UploadSessionStatus) -> UploadSessionRecord {
    let now = Timestamp::from_second(1_800_000_000).expect("in range");
    UploadSessionRecord {
        upload_id: UploadId::new(format!("reservation-{tag}")),
        asset_id: AssetId::new(format!("reservation-asset-{tag}")),
        owner_id: OwnerId::new("reservation-uploader".to_owned()),
        upload_user_id: uploader(),
        album_id: None,
        content_type: Some("application/octet-stream".to_owned()),
        expected_hash: hash(),
        crypto_suite_id: 1,
        protocol_version: "2026-08-29".to_owned(),
        blob_role: BlobRole::Original,
        intent_id: None,
        manifest_envelope: "{}".to_owned(),
        received_bytes: 0,
        total_size: BYTES.len() as u64,
        status,
        failure_code: None,
        created_at: now,
        last_progress_at: now,
    }
}

impl Harness {
    /// Charge [`BYTES`] to `user`, as session creation does.
    async fn charge(&self, user: &UserId) {
        let outcome = self
            .quotas
            .charge(
                user,
                &address(),
                BYTES.len() as u64,
                Timestamp::from_second(1_800_000_000).expect("in range"),
                QuotaLimits::unlimited(),
            )
            .await
            .expect("the ledger answers");
        assert!(matches!(outcome, ChargeOutcome::Charged { .. }));
    }

    async fn used(&self, user: &UserId) -> u64 {
        self.quotas
            .usage(user)
            .await
            .expect("the ledger answers")
            .used
    }
}

#[tokio::test]
async fn an_ended_session_gives_its_reservation_back() {
    let h = harness();
    h.charge(&uploader()).await;
    let ended = session("ended", UploadSessionStatus::FailedProcessing);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Released
    );
    assert_eq!(h.used(&uploader()).await, 0);
}

#[tokio::test]
async fn stored_bytes_keep_the_reservation() {
    let h = harness();
    h.charge(&uploader()).await;
    h.blobs.put(&address(), BYTES).await.expect("stored");
    let ended = session("stored", UploadSessionStatus::FailedProcessing);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::BytesStored
    );
    assert_eq!(
        h.used(&uploader()).await,
        BYTES.len() as u64,
        "the attribution pays for the stored blob, and goes only when the collector removes it"
    );
}

#[tokio::test]
async fn another_live_session_of_the_uploader_keeps_the_reservation() {
    let h = harness();
    h.charge(&uploader()).await;
    h.context
        .sessions()
        .open(session("live", UploadSessionStatus::Uploading))
        .await
        .expect("opened");
    let ended = session("cancelled", UploadSessionStatus::Pending);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::StillReserved
    );
    assert_eq!(h.used(&uploader()).await, BYTES.len() as u64);
}

#[tokio::test]
async fn the_ended_session_itself_never_keeps_its_own_reservation() {
    let h = harness();
    h.charge(&uploader()).await;
    // Terminal, and still readable to its cap — the state `FailedProcessing` leaves.
    let failed = session("failed", UploadSessionStatus::FailedProcessing);
    h.context
        .sessions()
        .open(failed.clone())
        .await
        .expect("opened");

    assert_eq!(
        release(&h.context, &failed).await.expect("answers"),
        Released::Released
    );
}

#[tokio::test]
async fn another_accounts_attribution_is_never_given_back() {
    let h = harness();
    let other = UserId::new("reservation-other".to_owned());
    h.charge(&other).await;
    let ended = session("merged", UploadSessionStatus::FailedProcessing);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::NotHeld
    );
    assert_eq!(h.used(&other).await, BYTES.len() as u64);
}

#[tokio::test]
async fn a_declared_hash_that_is_no_address_releases_nothing() {
    let h = harness();
    let mut ended = session("malformed", UploadSessionStatus::FailedProcessing);
    ended.expected_hash = "not-an-address".to_owned();

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Unaddressable
    );
}

#[tokio::test]
async fn a_session_that_opens_during_the_release_gets_the_reservation_back() {
    let h = racing_harness(session("arrived", UploadSessionStatus::Pending));
    h.charge(&uploader()).await;
    let ended = session("cancelled", UploadSessionStatus::Pending);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Restored
    );
    assert_eq!(
        h.used(&uploader()).await,
        BYTES.len() as u64,
        "the session that opened will store the bytes, so their reservation stands"
    );
}
