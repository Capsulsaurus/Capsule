//! Deterministic in-memory adapters — **test doubles**, never a deployment mode.
//!
//! Valkey is required; the server refuses to boot without `VALKEY_URL`
//! (design/filesystem/server.md, "Required Services"). What these types are for is letting the
//! rest of the rebuild be tested without a container, which is the acceptance gap
//! design/module-map.md sets for Kynos — and they earn that only by passing the same
//! [`super::conformance`] suite a live backend does.
//!
//! Two properties make them *deterministic* rather than merely fast:
//!
//! - **Expiry is real and clock-driven.** The Salvo `InMemorySessionStorage` ignored TTL
//!   outright, which is why every ceremony that rode it grew a second, redundant `expires_at`
//!   field to have something testable. These honour their store's TTL against an injected
//!   [`Clock`], so [`ManualClock`] tests expiry exactly, with no sleeping and no flake.
//! - **Every listing is sorted.** Iteration order is never a source of test noise, and the
//!   order asserted here is the one the port contracts, so a set-backed adapter must sort to
//!   match rather than the suite being loosened to accept its order.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::{SignedDuration, Timestamp};

use super::auth::{
    AuthStateStore, CohortRecord, CohortStore, DEFAULT_SESSION_HARD_CAP, DEFAULT_SESSION_TTL,
    SessionRecord,
};
use super::ceremony::{
    CHALLENGE_TTL, ChallengeStore, ChannelStore, DEVICE_SLOW_DOWN_STEP, DevicePollSlot, Direction,
    DrainOutcome, ENROLLMENT_CODE_TTL, EnrollmentStore, OIDC_AUTHORIZATION_TTL, OIDC_DEVICE_TTL,
    OidcAuthorizationStore, OidcDeviceStore, PendingAuthorization, PendingDeviceAuthorization,
    PendingEnrollment, RELAY_CHANNEL_TTL, RelayChannel, RelayOutcome, RelayPayload,
    RevokeAllChallenge,
};
use super::ids::{
    AlbumId, ChallengeToken, ChannelId, EnrollmentCode, OidcDeviceHandle, OidcState, SessionId,
    UploadId, UserId,
};
use super::upload::{
    AcceptedChunk, FinalizeClaim, LIFETIME_CAP, UploadSessionRecord, UploadSessionStatus,
    UploadSessionStore,
};
use super::{Clock, StoreError, StoreFuture, deadline};

/// Take a lock, recovering rather than propagating a poisoned one.
///
/// A test double must not turn one failed assertion inside a lock into a cascade of unrelated
/// failures, and `unwrap()` is denied workspace-wide besides. The data behind these locks is
/// plain records with no invariant a panic could have half-broken.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A clock the test drives.
///
/// The reason the conformance suite can assert TTL behaviour in a unit test: `advance` moves
/// every store sharing this clock past its expiry deterministically, with no `sleep`.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<Mutex<Timestamp>>,
}

impl ManualClock {
    /// A clock reading `start`.
    pub fn new(start: Timestamp) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Move the clock forward.
    pub fn advance(&self, by: SignedDuration) {
        let mut now = lock(&self.now);
        *now = deadline(*now, by);
    }
}

impl Default for ManualClock {
    /// Starts at the Unix epoch, so a test's absolute timestamps are readable in a failure.
    fn default() -> Self {
        Self::new(Timestamp::UNIX_EPOCH)
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *lock(&self.now)
    }
}

/// A stored record and the instant it stops being visible.
#[derive(Debug, Clone)]
struct Entry<T> {
    record: T,
    expires_at: Timestamp,
}

impl<T> Entry<T> {
    fn is_live_at(&self, now: Timestamp) -> bool {
        now < self.expires_at
    }
}

// ===========================================================================================
// Auth state
// ===========================================================================================

/// In-memory [`AuthStateStore`].
///
/// The per-user index lives in a private field with no method of its own. Every operation that
/// touches `sessions` touches `by_user` in the same critical section, and — the part that makes
/// a stale entry *unobservable* rather than merely unlikely — every read resolves index entries
/// through `sessions` and drops any that do not resolve. So even a hypothetical bug that left
/// an orphan id in `by_user` could not make a revoke-all report it.
#[derive(Debug)]
pub struct InMemoryAuthState {
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    hard_cap: SignedDuration,
    state: Mutex<AuthState>,
}

#[derive(Debug, Default)]
struct AuthState {
    sessions: BTreeMap<SessionId, Entry<SessionRecord>>,
    by_user: BTreeMap<UserId, BTreeSet<SessionId>>,
}

impl AuthState {
    /// Drop everything past its expiry, from both halves at once.
    fn purge(&mut self, now: Timestamp) {
        let expired: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, entry)| !entry.is_live_at(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(entry) = self.sessions.remove(&id) {
                self.unindex(&entry.record.user_id, &id);
            }
        }
    }

    fn unindex(&mut self, user: &UserId, session: &SessionId) {
        if let Some(ids) = self.by_user.get_mut(user) {
            ids.remove(session);
            if ids.is_empty() {
                self.by_user.remove(user);
            }
        }
    }

    /// Resolve a user's index entries into records, dropping any that do not resolve.
    fn resolve(&self, user: &UserId) -> Vec<SessionRecord> {
        let Some(ids) = self.by_user.get(user) else {
            return Vec::new();
        };
        let mut found: Vec<SessionRecord> = ids
            .iter()
            .filter_map(|id| self.sessions.get(id))
            .map(|entry| entry.record.clone())
            .collect();
        found.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        found
    }
}

impl InMemoryAuthState {
    /// A store on `clock` with the given sliding window and [`DEFAULT_SESSION_HARD_CAP`].
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            hard_cap: DEFAULT_SESSION_HARD_CAP,
            state: Mutex::new(AuthState::default()),
        }
    }

    /// The same store with its hard cap set to `hard_cap`.
    #[must_use]
    pub fn with_hard_cap(mut self, hard_cap: SignedDuration) -> Self {
        self.hard_cap = hard_cap;
        self
    }

    /// A store on `clock` with [`DEFAULT_SESSION_TTL`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, DEFAULT_SESSION_TTL)
    }

    /// Store `record` until `expires_at`, indexed under its user: the one write both
    /// [`AuthStateStore::open_session`] and [`AuthStateStore::restore_session`] make.
    fn store_session(
        &self,
        record: SessionRecord,
        expires_at: Timestamp,
        verb: &'static str,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);

            let session_id = record.session_id.clone();
            let user_id = record.user_id.clone();
            let entry = Entry { record, expires_at };
            // Re-opening an id that already named a *different* user must not leave the old
            // user's listing pointing at a session that is now someone else's.
            if let Some(previous) = state.sessions.insert(session_id.clone(), entry)
                && previous.record.user_id != user_id
            {
                state.unindex(&previous.record.user_id, &session_id);
            }
            state
                .by_user
                .entry(user_id.clone())
                .or_default()
                .insert(session_id.clone());

            tracing::debug!(%session_id, %user_id, %expires_at, verb, "stored session");
            Ok(())
        })
    }
}

impl AuthStateStore for InMemoryAuthState {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn hard_cap(&self) -> SignedDuration {
        self.hard_cap
    }

    fn open_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        let expires_at = self.expires_at(&record, self.clock.now());
        self.store_session(record, expires_at, "opened")
    }

    fn restore_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        let expires_at = self.expires_at(&record, record.created_at);
        self.store_session(record, expires_at, "restored")
    }

    fn read_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let found = state
                .sessions
                .get(session)
                .map(|entry| entry.record.clone());
            tracing::trace!(%session, hit = found.is_some(), "read session");
            Ok(found)
        })
    }

    fn touch_session<'a>(
        &'a self,
        session: &'a SessionId,
        last_active_at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let Some(entry) = state.sessions.get_mut(session) else {
                tracing::trace!(%session, "touch found no live session");
                return Ok(None);
            };
            entry.record.last_active_at = last_active_at;
            tracing::trace!(%session, "touched session");
            Ok(Some(entry.record.clone()))
        })
    }

    fn mark_authenticated<'a>(
        &'a self,
        session: &'a SessionId,
        at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let Some(entry) = state.sessions.get_mut(session) else {
                tracing::trace!(%session, "re-authentication found no live session");
                return Ok(None);
            };
            entry.record.authenticated_at = at;
            tracing::info!(%session, "a session re-authenticated");
            Ok(Some(entry.record.clone()))
        })
    }

    fn close_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let Some(entry) = state.sessions.remove(session) else {
                tracing::debug!(%session, "close found no live session");
                return Ok(None);
            };
            state.unindex(&entry.record.user_id, session);
            tracing::info!(%session, user_id = %entry.record.user_id, "closed session");
            Ok(Some(entry.record))
        })
    }

    fn sessions_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let found = state.resolve(user);
            tracing::trace!(%user, count = found.len(), "listed sessions for user");
            Ok(found)
        })
    }

    fn close_all_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);

            // Resolve first: the answer is the records that existed, so the count reported to
            // the user cannot be anything other than the number actually removed.
            let removed = state.resolve(user);
            for record in &removed {
                state.sessions.remove(&record.session_id);
            }
            state.by_user.remove(user);

            tracing::info!(%user, revoked = removed.len(), "revoked every session for user");
            Ok(removed)
        })
    }
}

// ===========================================================================================
// Upload sessions
// ===========================================================================================

/// In-memory [`UploadSessionStore`].
///
/// Both views the Salvo adapter maintained as separate Valkey keys — the uploader index and
/// the progress sorted-set — are *derived* here: the uploader index from the record's
/// `upload_user_id`, the progress order from its `last_progress_at`. A derived view cannot
/// drift from the records it is derived from.
#[derive(Debug)]
pub struct InMemoryUploadSessions {
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    state: Mutex<UploadState>,
}

#[derive(Debug, Default)]
struct UploadState {
    sessions: BTreeMap<UploadId, Entry<UploadSessionRecord>>,
    chunks: BTreeMap<UploadId, BTreeMap<u64, AcceptedChunk>>,
}

impl UploadState {
    fn purge(&mut self, now: Timestamp) {
        let expired: Vec<UploadId> = self
            .sessions
            .iter()
            .filter(|(_, entry)| !entry.is_live_at(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.sessions.remove(&id);
            self.chunks.remove(&id);
        }
    }

    /// A live, still-active session, or `None`.
    fn active_mut(&mut self, upload: &UploadId) -> Option<&mut UploadSessionRecord> {
        self.sessions
            .get_mut(upload)
            .map(|entry| &mut entry.record)
            .filter(|record| record.status.is_active())
    }
}

impl InMemoryUploadSessions {
    /// A store on `clock` with the given lifetime cap.
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            state: Mutex::new(UploadState::default()),
        }
    }

    /// A store on `clock` with the [`LIFETIME_CAP`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, LIFETIME_CAP)
    }
}

impl UploadSessionStore for InMemoryUploadSessions {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn open(&self, record: UploadSessionRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let upload_id = record.upload_id.clone();
            let uploader = record.upload_user_id.clone();
            state.sessions.insert(
                upload_id.clone(),
                Entry {
                    record,
                    expires_at: deadline(now, self.ttl),
                },
            );
            tracing::debug!(%upload_id, %uploader, "opened upload session");
            Ok(())
        })
    }

    fn read<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let found = state.sessions.get(upload).map(|entry| entry.record.clone());
            tracing::trace!(%upload, hit = found.is_some(), "read upload session");
            Ok(found)
        })
    }

    fn sessions_for_uploader<'a>(
        &'a self,
        uploader: &'a UserId,
    ) -> StoreFuture<'a, Vec<UploadSessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let mut found: Vec<UploadSessionRecord> = state
                .sessions
                .values()
                .map(|entry| &entry.record)
                .filter(|record| &record.upload_user_id == uploader)
                .cloned()
                .collect();
            found.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.upload_id.cmp(&b.upload_id))
            });
            tracing::trace!(%uploader, count = found.len(), "listed upload sessions");
            Ok(found)
        })
    }

    fn record_progress<'a>(
        &'a self,
        upload: &'a UploadId,
        chunk: AcceptedChunk,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let Some(record) = state.active_mut(upload) else {
                tracing::debug!(%upload, "progress found no active session");
                return Ok(None);
            };
            record.received_bytes = chunk.next_offset;
            record.last_progress_at = chunk.accepted_at;
            if record.status == UploadSessionStatus::Pending {
                record.status = UploadSessionStatus::Uploading;
            }
            let updated = record.clone();
            state
                .chunks
                .entry(upload.clone())
                .or_default()
                .insert(chunk.offset, chunk);
            tracing::debug!(
                %upload,
                received_bytes = updated.received_bytes,
                "accepted chunk"
            );
            Ok(Some(updated))
        })
    }

    fn chunk_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
    ) -> StoreFuture<'a, Option<AcceptedChunk>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let found = state
                .chunks
                .get(upload)
                .and_then(|chunks| chunks.get(&offset))
                .cloned();
            tracing::trace!(%upload, offset, hit = found.is_some(), "chunk replay lookup");
            Ok(found)
        })
    }

    fn reconcile_received_bytes<'a>(
        &'a self,
        upload: &'a UploadId,
        on_disk: u64,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let Some(entry) = state.sessions.get_mut(upload) else {
                return Ok(None);
            };
            let was = entry.record.received_bytes;
            entry.record.received_bytes = on_disk;
            tracing::info!(%upload, was, now = on_disk, "reconciled received bytes to disk");
            Ok(Some(entry.record.clone()))
        })
    }

    fn set_status<'a>(
        &'a self,
        upload: &'a UploadId,
        status: UploadSessionStatus,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let Some(entry) = state.sessions.get_mut(upload) else {
                return Ok(None);
            };
            entry.record.status = status;
            tracing::debug!(%upload, status = status.as_str(), "set upload status");
            Ok(Some(entry.record.clone()))
        })
    }

    fn fail_processing<'a>(
        &'a self,
        upload: &'a UploadId,
        failure_code: &'a str,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let Some(entry) = state.sessions.get_mut(upload) else {
                return Ok(None);
            };
            entry.record.status = UploadSessionStatus::FailedProcessing;
            entry.record.failure_code = Some(failure_code.to_owned());
            tracing::debug!(%upload, failure_code, "failed an upload session");
            Ok(Some(entry.record.clone()))
        })
    }

    fn claim_finalize<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, FinalizeClaim> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let Some(entry) = state.sessions.get_mut(upload) else {
                tracing::debug!(%upload, "finalize claim found no session");
                return Ok(FinalizeClaim::NotFound);
            };
            if !matches!(
                entry.record.status,
                UploadSessionStatus::Pending | UploadSessionStatus::Uploading
            ) {
                tracing::debug!(
                    %upload,
                    status = entry.record.status.as_str(),
                    "finalize already claimed"
                );
                return Ok(FinalizeClaim::AlreadyClaimed);
            }
            entry.record.status = UploadSessionStatus::WaitingForProcessing;
            tracing::info!(%upload, "claimed finalization");
            Ok(FinalizeClaim::Won(Box::new(entry.record.clone())))
        })
    }

    fn claimed(&self) -> StoreFuture<'_, Vec<UploadSessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let mut found: Vec<UploadSessionRecord> = state
                .sessions
                .values()
                .map(|entry| &entry.record)
                .filter(|record| record.status == UploadSessionStatus::WaitingForProcessing)
                .cloned()
                .collect();
            found.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.upload_id.cmp(&b.upload_id))
            });
            tracing::debug!(count = found.len(), "listed claimed upload sessions");
            Ok(found)
        })
    }

    fn discard<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            state.chunks.remove(upload);
            let removed = state.sessions.remove(upload).map(|entry| entry.record);
            tracing::info!(%upload, hit = removed.is_some(), "discarded upload session");
            Ok(removed)
        })
    }

    fn pending_for_address<'a>(
        &'a self,
        uploader: &'a UserId,
        expected_hash: &'a str,
    ) -> StoreFuture<'a, Option<UploadId>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            // A scan, and only defensible because this adapter is a double — a real one indexes
            // `(upload_user_id, expected_hash)`. Ordered by upload id so two sessions declaring
            // the same bytes give a deterministic answer rather than a map-iteration one.
            let mut matches: Vec<&UploadId> = state
                .sessions
                .values()
                .map(|entry| &entry.record)
                .filter(|record| {
                    record.status.is_active()
                        && &record.upload_user_id == uploader
                        && record.expected_hash == expected_hash
                })
                .map(|record| &record.upload_id)
                .collect();
            matches.sort();
            Ok(matches.first().map(|id| (*id).clone()))
        })
    }

    fn in_flight_for_album<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, u64> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            Ok(state
                .sessions
                .values()
                .map(|entry| &entry.record)
                .filter(|record| {
                    record.status.is_active() && record.album_id.as_ref() == Some(album)
                })
                .count() as u64)
        })
    }

    fn least_recently_progressed(
        &self,
        not_progressed_since: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<UploadId>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let mut candidates: Vec<&UploadSessionRecord> = state
                .sessions
                .values()
                .map(|entry| &entry.record)
                .filter(|record| {
                    record.status.is_evictable() && record.last_progress_at < not_progressed_since
                })
                .collect();
            candidates.sort_by(|a, b| {
                a.last_progress_at
                    .cmp(&b.last_progress_at)
                    .then_with(|| a.upload_id.cmp(&b.upload_id))
            });
            let picked: Vec<UploadId> = candidates
                .into_iter()
                .take(limit)
                .map(|record| record.upload_id.clone())
                .collect();
            tracing::debug!(count = picked.len(), "listed eviction candidates");
            Ok(picked)
        })
    }
}

// ===========================================================================================
// Device cohorts
// ===========================================================================================

/// In-memory [`CohortStore`].
///
/// **No clock and no TTL.** Every other store in this module expires things; this one is the
/// exception that makes the cohort story work at all. A cohort is worth recording precisely
/// because it outlives the sessions that carried it — a user reinstalls, gets a new `device_id`
/// by design, and the sessions that named the old one expired months ago. A map that expired
/// with them would forget exactly when "have I seen this device before?" starts being worth
/// asking.
#[derive(Debug, Default)]
pub struct InMemoryCohorts {
    seen: Mutex<BTreeMap<(UserId, String), CohortRecord>>,
}

impl InMemoryCohorts {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }
}

impl CohortStore for InMemoryCohorts {
    fn observe<'a>(
        &'a self,
        user: &'a UserId,
        cohort_hash: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, CohortRecord> {
        Box::pin(async move {
            let mut seen = lock(&self.seen);
            let key = (user.clone(), cohort_hash.to_owned());
            let record = seen
                .entry(key)
                .and_modify(|held| held.last_seen = at)
                .or_insert_with(|| {
                    tracing::info!(%user, "an account was seen under a new device cohort");
                    CohortRecord {
                        user_id: user.clone(),
                        cohort_hash: cohort_hash.to_owned(),
                        first_seen: at,
                        last_seen: at,
                    }
                });
            Ok(record.clone())
        })
    }

    fn cohorts_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<CohortRecord>> {
        Box::pin(async move {
            let seen = lock(&self.seen);
            let mut found: Vec<CohortRecord> = seen
                .iter()
                .filter(|((held, _), _)| held == user)
                .map(|(_, record)| record.clone())
                .collect();
            // Oldest first sighting first, ties broken by the hash so the order is total. A
            // user-visible listing whose order depends on the backend is a listing that
            // reshuffles itself between page loads.
            found.sort_by(|a, b| {
                a.first_seen
                    .cmp(&b.first_seen)
                    .then_with(|| a.cohort_hash.cmp(&b.cohort_hash))
            });
            Ok(found)
        })
    }
}

// ===========================================================================================
// Ceremonies
// ===========================================================================================

/// In-memory [`ChallengeStore`].
#[derive(Debug)]
pub struct InMemoryChallenges {
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    state: Mutex<BTreeMap<ChallengeToken, Entry<RevokeAllChallenge>>>,
}

impl InMemoryChallenges {
    /// A store on `clock` with the given challenge lifetime.
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            state: Mutex::new(BTreeMap::new()),
        }
    }

    /// A store on `clock` with the [`CHALLENGE_TTL`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, CHALLENGE_TTL)
    }
}

impl ChallengeStore for InMemoryChallenges {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn issue<'a>(
        &'a self,
        token: &'a ChallengeToken,
        record: RevokeAllChallenge,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let user_id = record.user_id.clone();
            lock(&self.state).insert(
                token.clone(),
                Entry {
                    record,
                    expires_at: deadline(now, self.ttl),
                },
            );
            tracing::info!(%user_id, "issued revoke-all challenge");
            Ok(())
        })
    }

    fn consume<'a>(
        &'a self,
        token: &'a ChallengeToken,
    ) -> StoreFuture<'a, Option<RevokeAllChallenge>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            // Burned on every attempt, live or not: there is no read that leaves it behind.
            let taken = state.remove(token).filter(|entry| entry.is_live_at(now));
            tracing::debug!(hit = taken.is_some(), "consumed revoke-all challenge");
            Ok(taken.map(|entry| entry.record))
        })
    }
}

/// How many pending OIDC authorizations the in-memory store will hold at once.
///
/// Ten thousand: at the ten-minute TTL that is a thousand begun-and-abandoned ceremonies a
/// minute before anything is refused, which is far beyond a self-hosted deployment's sign-in
/// rate and well inside the memory a record of four short strings costs. The ceiling exists so
/// that a caller who begins ceremonies without ever finishing them grows this map to a bound and
/// not to the heap. [`ValkeyOidcAuthorizations`](super::valkey::ValkeyOidcAuthorizations) has no
/// ceiling: its records carry a `PEXPIRE`, and the authorize route's per-host budget bounds the
/// rate they are written at — see that adapter's docs.
pub const PENDING_AUTHORIZATION_CEILING: usize = 10_000;

/// In-memory [`OidcAuthorizationStore`].
///
/// Expired records are purged on every `begin`, so the map holds live ceremonies plus whatever
/// expired since the last one — never everything ever begun — and a full map answers
/// [`StoreError::Rejected`], which the route renders as a `503`.
#[derive(Debug)]
pub struct InMemoryOidcAuthorizations {
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    ceiling: usize,
    state: Mutex<BTreeMap<OidcState, Entry<PendingAuthorization>>>,
}

impl InMemoryOidcAuthorizations {
    /// A store on `clock` with the given authorization lifetime and the default ceiling.
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            ceiling: PENDING_AUTHORIZATION_CEILING,
            state: Mutex::new(BTreeMap::new()),
        }
    }

    /// A store on `clock` with the [`OIDC_AUTHORIZATION_TTL`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, OIDC_AUTHORIZATION_TTL)
    }

    /// The same store holding at most `ceiling` pending ceremonies.
    #[must_use]
    pub fn with_ceiling(mut self, ceiling: usize) -> Self {
        self.ceiling = ceiling;
        self
    }

    /// Drop every record past its deadline.
    fn purge(state: &mut BTreeMap<OidcState, Entry<PendingAuthorization>>, now: Timestamp) {
        state.retain(|_, entry| entry.is_live_at(now));
    }
}

impl OidcAuthorizationStore for InMemoryOidcAuthorizations {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn begin<'a>(
        &'a self,
        state: &'a OidcState,
        record: PendingAuthorization,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut held = lock(&self.state);
            Self::purge(&mut held, now);
            if held.len() >= self.ceiling && !held.contains_key(state) {
                tracing::warn!(
                    pending = held.len(),
                    ceiling = self.ceiling,
                    "the pending OIDC authorization store is full; a ceremony was refused"
                );
                return Err(StoreError::Rejected {
                    store: "oidc authorizations",
                    detail: format!("{} pending ceremonies is the ceiling", self.ceiling),
                });
            }
            held.insert(
                state.clone(),
                Entry {
                    record,
                    expires_at: deadline(now, self.ttl),
                },
            );
            tracing::debug!("recorded a pending OIDC authorization");
            Ok(())
        })
    }

    fn consume<'a>(
        &'a self,
        state: &'a OidcState,
    ) -> StoreFuture<'a, Option<PendingAuthorization>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut held = lock(&self.state);
            // Burned on every attempt, live or not: a replayed `state` finds nothing.
            let taken = held.remove(state).filter(|entry| entry.is_live_at(now));
            tracing::debug!(
                hit = taken.is_some(),
                "consumed a pending OIDC authorization"
            );
            Ok(taken.map(|entry| entry.record))
        })
    }
}

/// How many pending OIDC device authorizations the in-memory store will hold at once.
///
/// The same ten thousand as [`PENDING_AUTHORIZATION_CEILING`], for the same reason: the begin is
/// an unauthenticated write, budgeted deployment-wide at sixty a minute, so at the fifteen-minute
/// lifetime the budget alone holds the live set under a thousand, and this is the bound that
/// holds when the budget is somehow not in front of it.
pub const PENDING_DEVICE_CEILING: usize = 10_000;

/// One held device ceremony: the record and the store's own poll stamp.
#[derive(Debug)]
struct HeldDevice {
    record: PendingDeviceAuthorization,
    polled_at: Timestamp,
}

/// In-memory [`OidcDeviceStore`] (issue #635).
///
/// The same shape as [`InMemoryOidcAuthorizations`] — purge on every begin, refuse past the
/// ceiling — plus the poll schedule, which one lock makes atomic.
#[derive(Debug)]
pub struct InMemoryOidcDevices {
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    ceiling: usize,
    state: Mutex<BTreeMap<OidcDeviceHandle, Entry<HeldDevice>>>,
}

impl InMemoryOidcDevices {
    /// A store on `clock` with the given ceremony lifetime and the default ceiling.
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            ceiling: PENDING_DEVICE_CEILING,
            state: Mutex::new(BTreeMap::new()),
        }
    }

    /// A store on `clock` with the [`OIDC_DEVICE_TTL`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, OIDC_DEVICE_TTL)
    }

    /// The same store holding at most `ceiling` pending ceremonies.
    #[must_use]
    pub fn with_ceiling(mut self, ceiling: usize) -> Self {
        self.ceiling = ceiling;
        self
    }
}

impl OidcDeviceStore for InMemoryOidcDevices {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn begin<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
        record: PendingDeviceAuthorization,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut held = lock(&self.state);
            held.retain(|_, entry| entry.is_live_at(now));
            if held.len() >= self.ceiling && !held.contains_key(handle) {
                tracing::warn!(
                    pending = held.len(),
                    ceiling = self.ceiling,
                    "the pending OIDC device store is full; a ceremony was refused"
                );
                return Err(StoreError::Rejected {
                    store: "oidc devices",
                    detail: format!("{} pending ceremonies is the ceiling", self.ceiling),
                });
            }
            held.insert(
                handle.clone(),
                Entry {
                    record: HeldDevice {
                        record,
                        polled_at: now,
                    },
                    expires_at: deadline(now, self.ttl),
                },
            );
            tracing::debug!("recorded a pending OIDC device authorization");
            Ok(())
        })
    }

    fn claim<'a>(&'a self, handle: &'a OidcDeviceHandle) -> StoreFuture<'a, DevicePollSlot> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut held = lock(&self.state);
            let Some(entry) = held.get_mut(handle).filter(|entry| entry.is_live_at(now)) else {
                tracing::debug!("claimed a poll on no live OIDC device authorization");
                return Ok(DevicePollSlot::Unknown);
            };
            let held = &mut entry.record;
            let due = deadline(held.polled_at, held.record.interval);
            held.polled_at = now;
            if now < due {
                held.record.interval += DEVICE_SLOW_DOWN_STEP;
                tracing::debug!(
                    interval_secs = held.record.interval.as_secs(),
                    "an OIDC device authorization was polled too soon"
                );
                return Ok(DevicePollSlot::TooSoon {
                    interval: held.record.interval,
                });
            }
            tracing::debug!("claimed a poll on an OIDC device authorization");
            Ok(DevicePollSlot::Due(held.record.clone()))
        })
    }

    fn slow_down<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
    ) -> StoreFuture<'a, Option<SignedDuration>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut held = lock(&self.state);
            let raised = held
                .get_mut(handle)
                .filter(|entry| entry.is_live_at(now))
                .map(|entry| {
                    entry.record.record.interval += DEVICE_SLOW_DOWN_STEP;
                    entry.record.record.interval
                });
            tracing::debug!(
                hit = raised.is_some(),
                "raised an OIDC device authorization's interval"
            );
            Ok(raised)
        })
    }

    fn finish<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
    ) -> StoreFuture<'a, Option<PendingDeviceAuthorization>> {
        Box::pin(async move {
            let now = self.clock.now();
            let taken = lock(&self.state)
                .remove(handle)
                .filter(|entry| entry.is_live_at(now));
            tracing::debug!(
                hit = taken.is_some(),
                "finished an OIDC device authorization"
            );
            Ok(taken.map(|entry| entry.record.record))
        })
    }
}

/// In-memory [`EnrollmentStore`].
///
/// Both spellings index the same record and are inserted and removed together, so one
/// spelling can never outlive the other.
#[derive(Debug)]
pub struct InMemoryEnrollments {
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    state: Mutex<BTreeMap<EnrollmentCode, Entry<PendingEnrollment>>>,
}

impl InMemoryEnrollments {
    /// A store on `clock` with the given code lifetime.
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            state: Mutex::new(BTreeMap::new()),
        }
    }

    /// A store on `clock` with the [`ENROLLMENT_CODE_TTL`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, ENROLLMENT_CODE_TTL)
    }
}

impl EnrollmentStore for InMemoryEnrollments {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn issue(&self, record: PendingEnrollment) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let expires_at = deadline(now, self.ttl);
            let user_id = record.user_id.clone();
            let code = record.code.clone();
            let fallback = record.text_fallback.clone();
            let mut state = lock(&self.state);
            state.insert(
                code,
                Entry {
                    record: record.clone(),
                    expires_at,
                },
            );
            state.insert(fallback, Entry { record, expires_at });
            tracing::info!(%user_id, "issued enrollment code");
            Ok(())
        })
    }

    fn is_taken<'a>(&'a self, code: &'a EnrollmentCode) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let now = self.clock.now();
            let taken = lock(&self.state)
                .get(code)
                .is_some_and(|entry| entry.is_live_at(now));
            tracing::trace!(taken, "checked enrollment code collision");
            Ok(taken)
        })
    }

    fn redeem<'a>(
        &'a self,
        code: &'a EnrollmentCode,
    ) -> StoreFuture<'a, Option<PendingEnrollment>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            let Some(entry) = state.remove(code) else {
                tracing::debug!("enrollment code unknown or already redeemed");
                return Ok(None);
            };
            // Whichever spelling was presented, the enrollment is gone in both.
            state.remove(&entry.record.code);
            state.remove(&entry.record.text_fallback);
            if !entry.is_live_at(now) {
                tracing::debug!("enrollment code expired — burned on this attempt");
                return Ok(None);
            }
            tracing::info!(user_id = %entry.record.user_id, "redeemed enrollment code");
            Ok(Some(entry.record))
        })
    }
}

/// In-memory [`ChannelStore`].
///
/// The mailboxes hang off the channel and are dropped with it — they carry no lifetime of
/// their own to get out of step with.
#[derive(Debug)]
pub struct InMemoryChannels {
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    state: Mutex<ChannelState>,
}

#[derive(Debug, Default)]
struct ChannelState {
    channels: BTreeMap<ChannelId, Entry<RelayChannel>>,
    mailboxes: BTreeMap<(ChannelId, Direction), Vec<RelayPayload>>,
}

impl ChannelState {
    fn purge(&mut self, now: Timestamp) {
        let expired: Vec<ChannelId> = self
            .channels
            .iter()
            .filter(|(_, entry)| !entry.is_live_at(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.drop_channel(&id);
        }
    }

    fn drop_channel(&mut self, channel: &ChannelId) -> bool {
        for direction in [Direction::ToInitiator, Direction::ToEnrollee] {
            self.mailboxes.remove(&(channel.clone(), direction));
        }
        self.channels.remove(channel).is_some()
    }
}

impl InMemoryChannels {
    /// A store on `clock` with the given channel lifetime.
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            state: Mutex::new(ChannelState::default()),
        }
    }

    /// A store on `clock` with the [`RELAY_CHANNEL_TTL`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, RELAY_CHANNEL_TTL)
    }
}

impl ChannelStore for InMemoryChannels {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn open<'a>(&'a self, channel: &'a ChannelId, record: RelayChannel) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let initiator = record.initiator_user_id.clone();
            state.channels.insert(
                channel.clone(),
                Entry {
                    record,
                    expires_at: deadline(now, self.ttl),
                },
            );
            tracing::info!(%channel, %initiator, "opened enrollment relay channel");
            Ok(())
        })
    }

    fn lookup<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, Option<RelayChannel>> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let found = state
                .channels
                .get(channel)
                .map(|entry| entry.record.clone());
            tracing::trace!(%channel, hit = found.is_some(), "looked up relay channel");
            Ok(found)
        })
    }

    fn enqueue<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
        payload: RelayPayload,
    ) -> StoreFuture<'a, RelayOutcome> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            // Liveness and the append share one critical section, so the channel cannot
            // expire between the check and the write.
            if !state.channels.contains_key(channel) {
                tracing::debug!(%channel, "relay send found no live channel");
                return Ok(RelayOutcome::NoChannel);
            }
            let payload_len = payload.len();
            let mailbox = state
                .mailboxes
                .entry((channel.clone(), direction))
                .or_default();
            mailbox.push(payload);
            let depth = mailbox.len();
            tracing::debug!(
                %channel,
                direction = direction.as_str(),
                payload_len,
                depth,
                "relayed opaque enrollment payload"
            );
            Ok(RelayOutcome::Enqueued { depth })
        })
    }

    fn drain<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
    ) -> StoreFuture<'a, DrainOutcome> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            if !state.channels.contains_key(channel) {
                tracing::debug!(%channel, "relay drain found no live channel");
                return Ok(DrainOutcome::NoChannel);
            }
            let drained = state
                .mailboxes
                .remove(&(channel.clone(), direction))
                .unwrap_or_default();
            tracing::debug!(
                %channel,
                direction = direction.as_str(),
                drained = drained.len(),
                "drained enrollment relay mailbox"
            );
            Ok(DrainOutcome::Drained(drained))
        })
    }

    fn close<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);
            let was_live = state.drop_channel(channel);
            tracing::info!(%channel, was_live, "closed enrollment relay channel");
            Ok(was_live)
        })
    }
}

// ===========================================================================================
// The harness the conformance suite drives
// ===========================================================================================

/// Every in-memory store on one shared [`ManualClock`].
///
/// Constructed with short lifetimes by default so the conformance suite's expiry cases move
/// the clock by a readable amount rather than by a day.
#[derive(Debug)]
pub struct InMemoryStores {
    clock: ManualClock,
    auth: InMemoryAuthState,
    uploads: InMemoryUploadSessions,
    challenges: InMemoryChallenges,
    enrollments: InMemoryEnrollments,
    channels: InMemoryChannels,
    /// The one store here with no TTL and no clock — see [`InMemoryCohorts`].
    cohorts: InMemoryCohorts,
    oidc_authorizations: InMemoryOidcAuthorizations,
    oidc_devices: InMemoryOidcDevices,
}

impl InMemoryStores {
    /// Every store with its production lifetime, on a fresh clock at the Unix epoch.
    pub fn new() -> Self {
        Self::with_ttl(
            ManualClock::default(),
            DEFAULT_SESSION_TTL,
            LIFETIME_CAP,
            CHALLENGE_TTL,
            ENROLLMENT_CODE_TTL,
            RELAY_CHANNEL_TTL,
            OIDC_AUTHORIZATION_TTL,
            OIDC_DEVICE_TTL,
        )
    }

    /// Every store on one lifetime, for the conformance suite's expiry cases.
    ///
    /// A single TTL across all of them is what lets [`super::conformance::CohortHarness::advance`] be
    /// one operation rather than five, and it is legitimate precisely because the TTL is a
    /// property of the *store instance* — varying it is configuration, not a per-call argument.
    pub fn with_uniform_ttl(ttl: SignedDuration) -> Self {
        Self::with_ttl(ManualClock::default(), ttl, ttl, ttl, ttl, ttl, ttl, ttl)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one lifetime per store, named in the order the stores are declared"
    )]
    fn with_ttl(
        clock: ManualClock,
        session: SignedDuration,
        upload: SignedDuration,
        challenge: SignedDuration,
        enrollment: SignedDuration,
        channel: SignedDuration,
        oidc_authorization: SignedDuration,
        oidc_device: SignedDuration,
    ) -> Self {
        let shared: Arc<dyn Clock> = Arc::new(clock.clone());
        Self {
            auth: InMemoryAuthState::new(Arc::clone(&shared), session),
            uploads: InMemoryUploadSessions::new(Arc::clone(&shared), upload),
            challenges: InMemoryChallenges::new(Arc::clone(&shared), challenge),
            enrollments: InMemoryEnrollments::new(Arc::clone(&shared), enrollment),
            channels: InMemoryChannels::new(Arc::clone(&shared), channel),
            cohorts: InMemoryCohorts::new(),
            oidc_authorizations: InMemoryOidcAuthorizations::new(
                Arc::clone(&shared),
                oidc_authorization,
            ),
            oidc_devices: InMemoryOidcDevices::new(Arc::clone(&shared), oidc_device),
            clock,
        }
    }

    /// The clock every store here reads.
    pub fn clock(&self) -> &ManualClock {
        &self.clock
    }
}

impl Default for InMemoryStores {
    fn default() -> Self {
        Self::new()
    }
}

impl super::conformance::CohortHarness for InMemoryStores {
    fn cohorts(&self) -> &dyn CohortStore {
        &self.cohorts
    }

    fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.clock.advance(by);
            Ok::<(), StoreError>(())
        })
    }
}

impl super::conformance::Harness for InMemoryStores {
    fn auth(&self) -> &dyn AuthStateStore {
        &self.auth
    }

    fn uploads(&self) -> &dyn UploadSessionStore {
        &self.uploads
    }

    fn challenges(&self) -> &dyn ChallengeStore {
        &self.challenges
    }

    fn enrollments(&self) -> &dyn EnrollmentStore {
        &self.enrollments
    }

    fn channels(&self) -> &dyn ChannelStore {
        &self.channels
    }

    fn oidc_authorizations(&self) -> &dyn OidcAuthorizationStore {
        &self.oidc_authorizations
    }

    fn oidc_devices(&self) -> &dyn OidcDeviceStore {
        &self.oidc_devices
    }
}

#[cfg(test)]
mod tests;
