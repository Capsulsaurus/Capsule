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

use super::auth::{AuthStateStore, CohortRecord, CohortStore, DEFAULT_SESSION_TTL, SessionRecord};
use super::ceremony::{
    AuthenticationCeremony, CHALLENGE_TTL, ChallengeStore, ChannelStore, Direction, DrainOutcome,
    ENROLLMENT_CODE_TTL, EnrollmentStore, PendingEnrollment, RELAY_CHANNEL_TTL,
    RegistrationCeremony, RelayChannel, RelayOutcome, RelayPayload, RevokeAllChallenge,
    WEBAUTHN_CEREMONY_TTL, WebauthnCeremonyStore,
};
use super::ids::{
    CeremonyId, ChallengeToken, ChannelId, EnrollmentCode, SessionId, UploadId, UserId,
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
    /// A store on `clock` with the given session lifetime.
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            state: Mutex::new(AuthState::default()),
        }
    }

    /// A store on `clock` with [`DEFAULT_SESSION_TTL`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, DEFAULT_SESSION_TTL)
    }
}

impl AuthStateStore for InMemoryAuthState {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn open_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let mut state = lock(&self.state);
            state.purge(now);

            let session_id = record.session_id.clone();
            let user_id = record.user_id.clone();
            let entry = Entry {
                record,
                expires_at: deadline(now, self.ttl),
            };
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

            tracing::debug!(%session_id, %user_id, "opened session");
            Ok(())
        })
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
                    record.status.is_active() && record.last_progress_at < not_progressed_since
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

/// In-memory [`WebauthnCeremonyStore`].
///
/// Two maps, not one namespaced by a key prefix: a registration and an authentication under
/// the same ceremony id are simply different entries in different maps, so the type confusion
/// the `passkey_reg:` / `passkey_auth:` convention existed to prevent has nowhere to occur.
#[derive(Debug)]
pub struct InMemoryWebauthnCeremonies {
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    registrations: Mutex<BTreeMap<CeremonyId, Entry<RegistrationCeremony>>>,
    authentications: Mutex<BTreeMap<CeremonyId, Entry<AuthenticationCeremony>>>,
}

impl InMemoryWebauthnCeremonies {
    /// A store on `clock` with the given ceremony window.
    pub fn new(clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            clock,
            ttl,
            registrations: Mutex::new(BTreeMap::new()),
            authentications: Mutex::new(BTreeMap::new()),
        }
    }

    /// A store on `clock` with the [`WEBAUTHN_CEREMONY_TTL`].
    pub fn with_default_ttl(clock: Arc<dyn Clock>) -> Self {
        Self::new(clock, WEBAUTHN_CEREMONY_TTL)
    }
}

impl WebauthnCeremonyStore for InMemoryWebauthnCeremonies {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn begin_registration<'a>(
        &'a self,
        ceremony: &'a CeremonyId,
        record: RegistrationCeremony,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let user_id = record.user_id.clone();
            lock(&self.registrations).insert(
                ceremony.clone(),
                Entry {
                    record,
                    expires_at: deadline(now, self.ttl),
                },
            );
            tracing::debug!(%user_id, "began passkey registration ceremony");
            Ok(())
        })
    }

    fn finish_registration<'a>(
        &'a self,
        ceremony: &'a CeremonyId,
    ) -> StoreFuture<'a, Option<RegistrationCeremony>> {
        Box::pin(async move {
            let now = self.clock.now();
            let taken = lock(&self.registrations)
                .remove(ceremony)
                .filter(|entry| entry.is_live_at(now));
            tracing::debug!(
                hit = taken.is_some(),
                "finished passkey registration ceremony"
            );
            Ok(taken.map(|entry| entry.record))
        })
    }

    fn begin_authentication<'a>(
        &'a self,
        ceremony: &'a CeremonyId,
        record: AuthenticationCeremony,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            lock(&self.authentications).insert(
                ceremony.clone(),
                Entry {
                    record,
                    expires_at: deadline(now, self.ttl),
                },
            );
            tracing::debug!("began passkey authentication ceremony");
            Ok(())
        })
    }

    fn finish_authentication<'a>(
        &'a self,
        ceremony: &'a CeremonyId,
    ) -> StoreFuture<'a, Option<AuthenticationCeremony>> {
        Box::pin(async move {
            let now = self.clock.now();
            let taken = lock(&self.authentications)
                .remove(ceremony)
                .filter(|entry| entry.is_live_at(now));
            tracing::debug!(
                hit = taken.is_some(),
                "finished passkey authentication ceremony"
            );
            Ok(taken.map(|entry| entry.record))
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
    webauthn: InMemoryWebauthnCeremonies,
    /// The one store here with no TTL and no clock — see [`InMemoryCohorts`].
    cohorts: InMemoryCohorts,
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
            WEBAUTHN_CEREMONY_TTL,
        )
    }

    /// Every store on one lifetime, for the conformance suite's expiry cases.
    ///
    /// A single TTL across all six is what lets [`super::conformance::Harness::advance`] be one
    /// operation rather than six, and it is legitimate precisely because the TTL is a property
    /// of the *store instance* — varying it is configuration, not a per-call argument.
    pub fn with_uniform_ttl(ttl: SignedDuration) -> Self {
        Self::with_ttl(ManualClock::default(), ttl, ttl, ttl, ttl, ttl, ttl)
    }

    #[allow(clippy::too_many_arguments)]
    fn with_ttl(
        clock: ManualClock,
        session: SignedDuration,
        upload: SignedDuration,
        challenge: SignedDuration,
        enrollment: SignedDuration,
        channel: SignedDuration,
        webauthn: SignedDuration,
    ) -> Self {
        let shared: Arc<dyn Clock> = Arc::new(clock.clone());
        Self {
            auth: InMemoryAuthState::new(Arc::clone(&shared), session),
            uploads: InMemoryUploadSessions::new(Arc::clone(&shared), upload),
            challenges: InMemoryChallenges::new(Arc::clone(&shared), challenge),
            enrollments: InMemoryEnrollments::new(Arc::clone(&shared), enrollment),
            channels: InMemoryChannels::new(Arc::clone(&shared), channel),
            webauthn: InMemoryWebauthnCeremonies::new(Arc::clone(&shared), webauthn),
            cohorts: InMemoryCohorts::new(),
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

    fn cohorts(&self) -> &dyn CohortStore {
        &self.cohorts
    }

    fn channels(&self) -> &dyn ChannelStore {
        &self.channels
    }

    fn webauthn(&self) -> &dyn WebauthnCeremonyStore {
        &self.webauthn
    }

    fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.clock.advance(by);
            Ok::<(), StoreError>(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::conformance;
    use super::*;

    /// A harness whose stores all die after a minute, so an expiry case advances a readable
    /// amount and the non-expiry cases never trip over it.
    fn harness() -> InMemoryStores {
        InMemoryStores::with_uniform_ttl(SignedDuration::from_mins(1))
    }

    /// Declares one `#[tokio::test]` per conformance case.
    ///
    /// One test each, on a fresh harness each: a failure names the property that broke, and no
    /// case can pass because a previous one left the store in a convenient state.
    macro_rules! conformance_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[tokio::test]
                async fn $case() {
                    conformance::$case(&harness()).await;
                }
            )+
        };
    }

    conformance_cases! {
        session_round_trips_every_field,
        reading_an_unknown_session_is_none,
        user_listing_is_ordered_and_scoped,
        closing_one_session_removes_it_from_the_user_listing,
        revoke_all_reports_only_sessions_actually_removed,
        closing_an_unknown_session_removes_nothing,
        reopening_a_session_id_does_not_duplicate_the_listing,
        touching_a_session_records_activity_without_extending_its_life,
        an_expired_session_leaves_no_listing_entry_behind,
        upload_session_round_trips_every_field,
        uploader_listing_is_ordered_and_scoped,
        recording_progress_advances_bytes_clock_and_replay_together,
        chunk_replay_is_offset_addressed,
        finalization_is_claimed_exactly_once,
        reconciling_received_bytes_does_not_move_the_progress_clock,
        a_terminal_session_is_not_an_eviction_candidate,
        discarding_removes_the_record_its_chunks_and_its_listing,
        eviction_candidates_are_ordered_by_progress_and_bounded,
        an_expired_upload_session_leaves_no_listing_entry_behind,
        a_challenge_is_single_use,
        a_challenge_expires_with_its_store,
        an_enrollment_redeems_by_either_spelling_and_burns_both,
        an_unissued_enrollment_code_is_not_taken,
        an_enrollment_expires_with_its_store,
        relaying_requires_a_live_channel,
        relayed_payloads_drain_in_order_and_by_direction,
        closing_a_channel_drops_both_mailboxes,
        webauthn_registration_and_authentication_do_not_collide,
        a_webauthn_ceremony_is_consumed_by_its_finish,
        a_webauthn_ceremony_expires_with_its_store,
    }

    /// The whole suite, in one pass on one harness.
    ///
    /// This is the entry point a container-backed adapter uses, so it is exercised here too —
    /// otherwise the first time anyone runs it would be against Valkey, where a failure is
    /// hardest to read. It also proves the cases really are independent: they share a harness
    /// and a clock that only moves forward.
    #[tokio::test]
    async fn the_whole_suite_passes_in_one_pass() {
        conformance::run_all(&harness()).await;
    }

    /// A store built with production lifetimes really carries them.
    ///
    /// The suite runs on a uniform short TTL, so without this nothing would notice a wiring
    /// mistake that gave every store the same lifetime in production too.
    #[test]
    fn production_lifetimes_are_per_ceremony() {
        let stores = InMemoryStores::new();
        assert_eq!(AuthStateStore::ttl(&stores.auth), DEFAULT_SESSION_TTL);
        assert_eq!(UploadSessionStore::ttl(&stores.uploads), LIFETIME_CAP);
        assert_eq!(ChallengeStore::ttl(&stores.challenges), CHALLENGE_TTL);
        assert_eq!(
            EnrollmentStore::ttl(&stores.enrollments),
            ENROLLMENT_CODE_TTL
        );
        assert_eq!(ChannelStore::ttl(&stores.channels), RELAY_CHANNEL_TTL);
        assert_eq!(
            WebauthnCeremonyStore::ttl(&stores.webauthn),
            WEBAUTHN_CEREMONY_TTL
        );
        assert_ne!(
            CHALLENGE_TTL, ENROLLMENT_CODE_TTL,
            "a ceremony's window belongs to what it is; if these ever coincide by accident \
             this assertion stops being evidence"
        );
    }

    /// The manual clock only moves when a test moves it.
    #[test]
    fn the_manual_clock_is_deterministic() {
        let clock = ManualClock::default();
        let start = clock.now();
        assert_eq!(
            clock.now(),
            start,
            "a manual clock does not tick on its own"
        );
        clock.advance(SignedDuration::from_secs(90));
        assert_eq!(clock.now(), deadline(start, SignedDuration::from_secs(90)));
    }
}
