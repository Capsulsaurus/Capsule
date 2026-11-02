//! When an ended session's reservation goes back, and the two holders that keep it.

use std::sync::Arc;

use capsule_core::crypto::hash::hash_bytes;
use jiff::Timestamp;
use uuid::Uuid;

use super::*;
use crate::blob::InMemoryBlobStore;
use crate::index::memory::InMemoryAssetIndex;
use crate::quota::{ChargeOutcome, InMemoryQuota, QuotaContext, QuotaLimits};
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

    fn reserve<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        until: Timestamp,
        limits: QuotaLimits,
    ) -> crate::store::StoreFuture<'a, ChargeOutcome> {
        self.inner.reserve(user, address, size, at, until, limits)
    }

    fn settle<'a>(&'a self, address: &'a ContentAddress) -> crate::store::StoreFuture<'a, bool> {
        self.inner.settle(address)
    }

    fn expired_reservations(
        &self,
        now: Timestamp,
        limit: usize,
    ) -> crate::store::StoreFuture<'_, Vec<crate::quota::ExpiredReservation>> {
        self.inner.expired_reservations(now, limit)
    }

    fn extend_reservation<'a>(
        &'a self,
        address: &'a ContentAddress,
        until: Timestamp,
    ) -> crate::store::StoreFuture<'a, bool> {
        self.inner.extend_reservation(address, until)
    }

    fn release_expired<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        now: Timestamp,
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
            self.inner.release_expired(user, address, now).await
        })
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
        sessions.clone(),
        blobs.clone(),
        Arc::new(InMemoryAssetIndex::new()),
        Arc::new(Unasked),
        QuotaContext::new(racing.clone(), clock.clone(), QuotaLimits::unlimited()),
        clock.clone(),
        UploadPolicy::default(),
    );
    Harness {
        context,
        quotas,
        ledger: racing,
        blobs,
        sessions,
        clock,
    }
}

struct Harness {
    context: UploadContext,
    quotas: Arc<InMemoryQuota>,
    /// The ledger the context charges through: `quotas` itself, or a racing wrapper over it.
    ledger: Arc<dyn crate::quota::QuotaStore>,
    blobs: Arc<InMemoryBlobStore>,
    sessions: Arc<InMemoryUploadSessions>,
    clock: Arc<ManualClock>,
}

fn harness() -> Harness {
    limited_harness(QuotaLimits::unlimited())
}

/// [`harness`], under `limits` rather than none.
fn limited_harness(limits: QuotaLimits) -> Harness {
    let clock = Arc::new(ManualClock::new(
        Timestamp::from_second(1_800_000_000).expect("in range"),
    ));
    let quotas = Arc::new(InMemoryQuota::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let sessions = Arc::new(InMemoryUploadSessions::with_default_ttl(clock.clone()));
    let context = UploadContext::new(
        sessions.clone(),
        blobs.clone(),
        Arc::new(InMemoryAssetIndex::new()),
        Arc::new(Unasked),
        QuotaContext::new(quotas.clone(), clock.clone(), limits),
        clock.clone(),
        UploadPolicy::default(),
    );
    Harness {
        context,
        ledger: quotas.clone(),
        quotas,
        blobs,
        sessions,
        clock,
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
        committed: crate::store::CommittedSet::default(),
        in_flight_cap: 1,
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

/// `session`, uploaded by `user` instead — another account sending the same bytes.
fn session_of(user: &UserId, tag: &str, status: UploadSessionStatus) -> UploadSessionRecord {
    let mut record = session(tag, status);
    record.upload_user_id = user.clone();
    record.owner_id = OwnerId::new(user.as_str());
    record
}

#[tokio::test]
async fn another_accounts_live_session_is_charged_when_the_holder_lets_go() {
    let h = harness();
    let other = UserId::new("reservation-other".to_owned());
    h.charge(&uploader()).await;
    // Opened long before the holder's session ends, so its own post-open re-charge found the
    // holder's attribution and debited nothing; no race with the release is involved.
    h.context
        .sessions()
        .open(session_of(&other, "theirs", UploadSessionStatus::Uploading))
        .await
        .expect("opened");
    let ended = session("cancelled", UploadSessionStatus::Pending);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Restored
    );
    assert_eq!(
        h.used(&uploader()).await,
        0,
        "the holder gave up the bytes, so they stop paying for them"
    );
    assert_eq!(
        h.used(&other).await,
        BYTES.len() as u64,
        "the session still sending the bytes is charged for them"
    );
}

#[tokio::test]
async fn another_accounts_ended_session_is_not_charged() {
    let h = harness();
    let other = UserId::new("reservation-other".to_owned());
    h.charge(&uploader()).await;
    h.context
        .sessions()
        .open(session_of(
            &other,
            "theirs",
            UploadSessionStatus::FailedProcessing,
        ))
        .await
        .expect("opened");
    let ended = session("cancelled", UploadSessionStatus::Pending);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Released
    );
    assert_eq!(h.used(&other).await, 0, "a terminal session stores nothing");
}

/// Limits under which an account that already uses `BYTES.len()` cannot take `BYTES` again.
fn tight() -> QuotaLimits {
    let size = BYTES.len() as u64;
    QuotaLimits::new(size, size * 2 - 1, crate::quota::DEFAULT_GRACE_WINDOW)
}

impl Harness {
    /// Charge `user` for `BYTES.len()` bytes at an address of their own, filling their quota.
    async fn fill(&self, user: &UserId) {
        let filler =
            ContentAddress::parse(&hash_bytes(user.as_str().as_bytes()).to_hex()).expect("address");
        self.quotas
            .charge(
                user,
                &filler,
                BYTES.len() as u64,
                Timestamp::from_second(1_800_000_000).expect("in range"),
                tight(),
            )
            .await
            .expect("the ledger answers");
    }

    /// Open `record` with a stage, as session creation does.
    async fn open(&self, record: UploadSessionRecord) {
        self.blobs.begin(&record.upload_id).await.expect("staged");
        self.context.sessions().open(record).await.expect("opened");
    }

    async fn status(&self, upload: &UploadId) -> (UploadSessionStatus, Option<String>) {
        let record = self
            .context
            .sessions()
            .read(upload)
            .await
            .expect("the store answers")
            .expect("the session is still readable");
        (record.status, record.failure_code)
    }

    /// Whether `upload` is gone from the session store.
    async fn discarded(&self, upload: &UploadId) -> bool {
        self.context
            .sessions()
            .read(upload)
            .await
            .expect("the store answers")
            .is_none()
    }

    /// Whether `upload`'s stage is gone: dropping it again finds nothing.
    async fn unstaged(&self, upload: &UploadId) -> bool {
        !self
            .blobs
            .abandon(upload)
            .await
            .expect("the blob store answers")
    }
}

#[tokio::test]
async fn another_accounts_session_over_its_hard_limit_is_discarded_not_charged() {
    let h = limited_harness(tight());
    let other = UserId::new("reservation-other".to_owned());
    h.charge(&uploader()).await;
    h.fill(&other).await;
    let theirs = session_of(&other, "theirs", UploadSessionStatus::Uploading);
    h.open(theirs.clone()).await;
    let ended = session("cancelled", UploadSessionStatus::Pending);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Released
    );
    assert_eq!(
        h.used(&other).await,
        BYTES.len() as u64,
        "the dedup never carries the other account over its hard limit"
    );
    assert!(
        h.discarded(&theirs.upload_id).await,
        "a session that cannot be charged may not go on to store its bytes"
    );
    assert!(
        h.unstaged(&theirs.upload_id).await,
        "its stage was already dropped"
    );
}

#[tokio::test]
async fn another_accounts_suspended_session_is_discarded_not_charged() {
    let h = harness();
    let other = UserId::new("reservation-other".to_owned());
    h.charge(&uploader()).await;
    h.quotas
        .suspend(
            &other,
            Timestamp::from_second(1_800_000_000).expect("in range"),
        )
        .await
        .expect("suspended");
    let theirs = session_of(&other, "theirs", UploadSessionStatus::Pending);
    h.open(theirs.clone()).await;
    let ended = session("cancelled", UploadSessionStatus::Pending);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Released
    );
    assert_eq!(
        h.used(&other).await,
        0,
        "a suspended account is never charged"
    );
    assert!(h.discarded(&theirs.upload_id).await);
    assert!(h.unstaged(&theirs.upload_id).await);
}

#[tokio::test]
async fn a_refused_session_hands_the_charge_to_the_next_live_one() {
    let h = limited_harness(tight());
    let full = UserId::new("reservation-full".to_owned());
    let roomy = UserId::new("reservation-roomy".to_owned());
    h.charge(&uploader()).await;
    h.fill(&full).await;
    // Listed smallest id first, so the full account's session is asked first.
    let refused = session_of(&full, "a-full", UploadSessionStatus::Uploading);
    let charged = session_of(&roomy, "b-roomy", UploadSessionStatus::Uploading);
    h.open(refused.clone()).await;
    h.open(charged.clone()).await;
    let ended = session("cancelled", UploadSessionStatus::Pending);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Restored
    );
    assert_eq!(h.used(&full).await, BYTES.len() as u64);
    assert_eq!(h.used(&roomy).await, BYTES.len() as u64);
    assert!(h.discarded(&refused.upload_id).await);
    assert_eq!(
        h.status(&charged.upload_id).await.0,
        UploadSessionStatus::Uploading
    );
}

#[tokio::test]
async fn a_finalizing_session_is_charged_past_its_limit_and_left_to_its_finalizer() {
    let h = limited_harness(tight());
    let other = UserId::new("reservation-other".to_owned());
    h.charge(&uploader()).await;
    h.fill(&other).await;
    let theirs = session_of(&other, "theirs", UploadSessionStatus::WaitingForProcessing);
    h.open(theirs.clone()).await;
    let ended = session("cancelled", UploadSessionStatus::Pending);

    assert_eq!(
        release(&h.context, &ended).await.expect("answers"),
        Released::Restored
    );
    assert_eq!(
        h.used(&other).await,
        2 * BYTES.len() as u64,
        "every byte is already on the server, so it is charged rather than stored free"
    );
    assert_eq!(
        h.status(&theirs.upload_id).await,
        (UploadSessionStatus::WaitingForProcessing, None),
        "the finalizer owns the session's terminal write"
    );
}

// ===========================================================================================
// Expiry: the reservation sweep, and settling at `Completed` (#1059)
// ===========================================================================================

/// Past every reservation made at the harness's start.
fn past_the_cap() -> jiff::SignedDuration {
    crate::store::upload::LIFETIME_CAP + jiff::SignedDuration::from_secs(1)
}

impl Harness {
    /// Reserve [`BYTES`] for `user`, as session creation does.
    async fn reserve(&self, user: &UserId) {
        let outcome = crate::quota::reserve_upload(
            self.context.quota(),
            user,
            &address(),
            BYTES.len() as u64,
        )
        .await
        .expect("the ledger answers");
        assert_eq!(outcome, crate::quota::UploadCharge::Admitted);
    }

    /// The sweep over this harness's ledger, blobs and sessions.
    fn sweep(&self) -> ReservationSweep {
        ReservationSweep::new(
            self.ledger.clone(),
            self.blobs.clone(),
            self.sessions.clone(),
            QuotaLimits::unlimited(),
        )
    }

    /// Run the sweep at the harness clock's now.
    async fn swept(&self) -> SweepReport {
        self.sweep()
            .sweep(crate::store::Clock::now(self.clock.as_ref()))
            .await
            .expect("the sweep answers")
    }
}

#[tokio::test]
async fn an_expired_sessions_reservation_is_released_by_the_sweep() {
    let h = harness();
    h.open(session("expires", UploadSessionStatus::Uploading))
        .await;
    h.reserve(&uploader()).await;
    h.clock.advance(past_the_cap());

    let report = h.swept().await;
    assert_eq!(report.released, vec![address()]);
    assert_eq!(h.used(&uploader()).await, 0);

    assert_eq!(
        h.swept().await,
        SweepReport::default(),
        "a released reservation is gone, so a second pass releases nothing"
    );
    assert_eq!(h.used(&uploader()).await, 0);
}

#[tokio::test]
async fn a_reservation_inside_its_lifetime_is_not_swept() {
    let h = harness();
    h.reserve(&uploader()).await;
    h.clock
        .advance(crate::store::upload::LIFETIME_CAP - jiff::SignedDuration::from_secs(1));

    assert_eq!(h.swept().await, SweepReport::default());
    assert_eq!(h.used(&uploader()).await, BYTES.len() as u64);
}

#[tokio::test]
async fn a_completed_uploads_charge_is_never_swept() {
    let h = harness();
    h.reserve(&uploader()).await;
    settle(
        &h.context,
        &session("completed", UploadSessionStatus::Completed),
    )
    .await;
    h.clock.advance(past_the_cap());

    assert_eq!(
        h.swept().await,
        SweepReport::default(),
        "a settled charge is not a reservation, so the sweep never lists it"
    );
    assert_eq!(h.used(&uploader()).await, BYTES.len() as u64);
}

#[tokio::test]
async fn a_committed_blob_settles_a_reservation_its_finalizer_left() {
    let h = harness();
    h.reserve(&uploader()).await;
    // Committed, and the finalizer died before settling.
    h.blobs.put(&address(), BYTES).await.expect("stored");
    h.clock.advance(past_the_cap());

    assert_eq!(h.swept().await.settled, vec![address()]);
    assert_eq!(h.used(&uploader()).await, BYTES.len() as u64);
    h.clock.advance(past_the_cap());
    assert_eq!(
        h.swept().await,
        SweepReport::default(),
        "once settled it is an ordinary charge, which only the collector releases"
    );
}

#[tokio::test]
async fn a_live_session_admitted_later_extends_the_reservation() {
    let h = harness();
    h.reserve(&uploader()).await;
    // A second session for the same bytes, opened late in the first one's lifetime.
    h.clock.advance(jiff::SignedDuration::from_hours(20));
    let other = UserId::new("reservation-other".to_owned());
    h.open(session_of(&other, "late", UploadSessionStatus::Uploading))
        .await;
    h.clock.advance(jiff::SignedDuration::from_hours(5));

    assert_eq!(h.swept().await.extended, vec![address()]);
    assert_eq!(h.used(&uploader()).await, BYTES.len() as u64);

    // The late session expires too, and nothing holds the reservation any more.
    h.clock.advance(past_the_cap());
    assert_eq!(h.swept().await.released, vec![address()]);
    assert_eq!(h.used(&uploader()).await, 0);
}

#[tokio::test]
async fn a_session_opening_as_the_sweep_releases_gets_the_reservation_back() {
    // Opened by the ledger after the sweep's live-session check and before its release.
    let late = Timestamp::from_second(1_800_000_000).expect("in range") + past_the_cap();
    let mut arrival = session("arrival", UploadSessionStatus::Pending);
    arrival.created_at = late;
    arrival.last_progress_at = late;
    let h = racing_harness(arrival);
    h.reserve(&uploader()).await;
    h.clock.advance(past_the_cap());

    assert_eq!(h.swept().await.restored, vec![address()]);
    assert_eq!(
        h.used(&uploader()).await,
        BYTES.len() as u64,
        "the session was admitted against the reservation, so it is put back"
    );
}

#[tokio::test]
async fn a_finalizer_whose_reservation_was_swept_charges_its_committed_bytes() {
    let h = harness();
    h.reserve(&uploader()).await;
    h.clock.advance(past_the_cap());
    // The sweep found no blob and released, and then the finalizer committed.
    assert_eq!(h.swept().await.released, vec![address()]);
    h.blobs.put(&address(), BYTES).await.expect("stored");

    settle(
        &h.context,
        &session("late-finalizer", UploadSessionStatus::WaitingForProcessing),
    )
    .await;
    assert_eq!(
        h.used(&uploader()).await,
        BYTES.len() as u64,
        "the committed bytes are charged, never stored free"
    );
    h.clock.advance(past_the_cap());
    assert_eq!(
        h.swept().await,
        SweepReport::default(),
        "and the charge is settled"
    );
}

#[tokio::test]
async fn the_sweep_reads_every_batch() {
    let h = harness();
    let other = UserId::new("reservation-other".to_owned());
    let theirs = ContentAddress::parse(&hash_bytes(b"other bytes").to_hex()).expect("address");
    h.reserve(&uploader()).await;
    crate::quota::reserve_upload(h.context.quota(), &other, &theirs, 7)
        .await
        .expect("the ledger answers");
    h.clock.advance(past_the_cap());

    let report = h
        .sweep()
        .with_batch(1)
        .sweep(crate::store::Clock::now(h.clock.as_ref()))
        .await
        .expect("the sweep answers");
    assert_eq!(report.released.len(), 2);
    assert_eq!(h.used(&uploader()).await, 0);
    assert_eq!(h.used(&other).await, 0);
}

#[tokio::test]
async fn an_applied_collection_pass_sweeps_expired_reservations() {
    let h = harness();
    h.reserve(&uploader()).await;
    h.clock.advance(past_the_cap());
    let housekeeping = UploadHousekeeping::new(h.sessions.clone(), h.sweep());

    crate::gc::ProgressSweep::sweep_progress(
        &housekeeping,
        crate::store::Clock::now(h.clock.as_ref()),
    )
    .await
    .expect("the housekeeping answers");
    assert_eq!(h.used(&uploader()).await, 0);
}

// ===========================================================================================
// Pressure discard
// ===========================================================================================

#[tokio::test]
async fn a_pressure_discard_releases_through_the_one_rule() {
    let h = harness();
    let victim = session("pressured", UploadSessionStatus::Uploading);
    h.open(victim.clone()).await;
    h.reserve(&uploader()).await;

    assert_eq!(
        discard_under_pressure(&h.context, &victim.upload_id)
            .await
            .expect("answers"),
        Some(Released::Released)
    );
    assert_eq!(h.used(&uploader()).await, 0);
    assert!(
        h.context
            .sessions()
            .read(&victim.upload_id)
            .await
            .expect("answers")
            .is_none()
    );
    assert_eq!(
        discard_under_pressure(&h.context, &victim.upload_id)
            .await
            .expect("answers"),
        None,
        "a session already discarded releases nothing a second time"
    );
}

#[tokio::test]
async fn a_pressure_discard_keeps_the_reservation_of_stored_bytes() {
    let h = harness();
    let victim = session("pressured-stored", UploadSessionStatus::Uploading);
    h.open(victim.clone()).await;
    h.reserve(&uploader()).await;
    h.blobs.put(&address(), BYTES).await.expect("stored");

    assert_eq!(
        discard_under_pressure(&h.context, &victim.upload_id)
            .await
            .expect("answers"),
        Some(Released::BytesStored)
    );
    assert_eq!(h.used(&uploader()).await, BYTES.len() as u64);
}

#[tokio::test]
async fn a_pressure_discard_leaves_a_claimed_session_to_its_finalizer() {
    let h = harness();
    let claimed = session("pressured-claimed", UploadSessionStatus::Uploading);
    h.open(claimed.clone()).await;
    h.reserve(&uploader()).await;
    h.context
        .sessions()
        .claim_finalize(&claimed.upload_id)
        .await
        .expect("claimed");

    assert_eq!(
        discard_under_pressure(&h.context, &claimed.upload_id)
            .await
            .expect("answers"),
        None
    );
    assert_eq!(h.used(&uploader()).await, BYTES.len() as u64);
}
