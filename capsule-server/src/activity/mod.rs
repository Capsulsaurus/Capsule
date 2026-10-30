//! The account activity log — the owner's record of what was done to and with their account.
//!
//! design/activity-log.md is the contract; this module is its port. The rules that shape it:
//!
//! - **A closed event set.** [`ActivityKind`] is every event the server writes. A new kind is an
//!   additive change on the wire, and a client renders one it does not know generically.
//! - **Identifiers, never content.** The server is end-to-end encrypted, so an event carries the
//!   ids the server already holds — the acting session, the session or link it is about — and
//!   nothing a client encrypted. There is no client address and no user agent: both outlive the
//!   session they describe, and adding a field later is additive where withdrawing one is not.
//! - **Append-only, bounded.** An event is never updated. It leaves the log only by the
//!   retention bound: [`RETENTION`] by age and [`MAX_EVENTS_PER_ACCOUNT`] by count, enforced by
//!   [`ActivityStore::record`] for the account it appends to, and by every read for age.
//! - **Recording never refuses the act.** [`ActivityContext::record`] is called after the action
//!   has happened and returns nothing: a store that cannot record logs the event at `error` and
//!   the action stands. A sign-out refused because the audit log was down is a security outage
//!   caused by an audit outage.
//! - **Recording never discloses an account.** A refused or locked sign-in is recorded only for
//!   an address that has an account, so its write must not lengthen that response:
//!   [`ActivityContext::record_detached`] takes it off the response path.
//! - **Tied to the request log.** Each event carries the request id
//!   [`crate::request_span`] minted for the request that caused it, which names that request's
//!   log lines — the detailed, short-lived half of the same story.
//!
//! # Adapters
//!
//! [`InMemoryActivity`] is the deterministic double and [`PostgresActivity`] the durable store;
//! both pass the one suite in [`conformance`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};

use crate::store::{Clock, SessionId, StoreError, StoreFuture, UserId};

/// How long an event stays in the log.
pub const RETENTION: SignedDuration = SignedDuration::from_hours(90 * 24);

/// The most events one account's log keeps; the oldest beyond it are deleted on the next append.
///
/// The bound a burst cannot exceed — a scripted loop against the second factor writes one event
/// per attempt — and the largest page the listing serves.
pub const MAX_EVENTS_PER_ACCOUNT: usize = 1_000;

/// The window inside which [`ActivityContext::record_detached`] records one event of a kind per
/// account, and coalesces every later one into it.
///
/// A refused or locked sign-in can be caused by anyone who knows an address, and the login route
/// charges no budget, so without this bound a scripted loop would push the account's whole log
/// out through [`MAX_EVENTS_PER_ACCOUNT`] in minutes. With it, the two detached kinds add at
/// most two events an hour to one account's log, and a flood needs weeks of uninterrupted
/// attempts to displace the rest. Every attempt is still in the request log.
pub const DETACHED_COALESCE_WINDOW: SignedDuration = SignedDuration::from_hours(1);

/// The most detached writes in flight at once, across every account.
///
/// Each one holds a store connection while it runs, from the pool every request shares, so a
/// password spray across many accounts must not be able to queue writes faster than the store
/// lands them. An event past the bound is dropped and logged whole at `error`.
pub const MAX_DETACHED_IN_FLIGHT: usize = 16;

/// What [`ActivityContext::record_detached`] did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detached {
    /// The event was stamped and its write started.
    Started,
    /// An event of the same kind for the same account was recorded inside
    /// [`DETACHED_COALESCE_WINDOW`]; this one is counted into it and not written.
    Coalesced,
    /// [`MAX_DETACHED_IN_FLIGHT`] writes were already running; the event is logged at `error`
    /// and not written.
    Dropped,
}

/// Every event the activity log records. The set is closed: nothing else is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityKind {
    /// An account was created.
    AccountRegistered,
    /// A password sign-in opened a session.
    SignInPassword,
    /// A password sign-in was refused: the password was wrong for this account.
    SignInPasswordRefused,
    /// A password sign-in was refused because this account is locked.
    SignInLocked,
    /// A sign-in completed its second factor and opened a session.
    SignInSecondFactor,
    /// A second-factor code was refused for a sign-in whose first factor had verified.
    SignInSecondFactorRefused,
    /// An identity-provider sign-in opened a session.
    SignInOidc,
    /// A session signed itself out.
    SignOut,
    /// A session revoked another session of the account.
    SessionRevoked,
    /// Every session was revoked with master-key proof.
    SessionsRevokedAll,
    /// The account password changed.
    PasswordChanged,
    /// A second factor was confirmed.
    TotpEnabled,
    /// A second factor was removed.
    TotpDisabled,
    /// The master-key escrow was stored over an earlier one, so the old recovery secret no
    /// longer opens the account.
    EscrowReplaced,
    /// A cross-device enrollment code was redeemed.
    DeviceEnrolled,
    /// A share link was created.
    ShareLinkCreated,
    /// A share link was revoked.
    ShareLinkRevoked,
    /// A guest-upload link was provisioned.
    DropLinkCreated,
    /// A guest-upload link was revoked.
    DropLinkRevoked,
}

impl ActivityKind {
    /// Every kind, in the order design/activity-log.md lists them.
    pub const ALL: [Self; 19] = [
        Self::AccountRegistered,
        Self::SignInPassword,
        Self::SignInPasswordRefused,
        Self::SignInLocked,
        Self::SignInSecondFactor,
        Self::SignInSecondFactorRefused,
        Self::SignInOidc,
        Self::SignOut,
        Self::SessionRevoked,
        Self::SessionsRevokedAll,
        Self::PasswordChanged,
        Self::TotpEnabled,
        Self::TotpDisabled,
        Self::EscrowReplaced,
        Self::DeviceEnrolled,
        Self::ShareLinkCreated,
        Self::ShareLinkRevoked,
        Self::DropLinkCreated,
        Self::DropLinkRevoked,
    ];

    /// The name this kind travels under, on the wire, in its column and in a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AccountRegistered => "account.registered",
            Self::SignInPassword => "sign_in.password",
            Self::SignInPasswordRefused => "sign_in.password_refused",
            Self::SignInLocked => "sign_in.locked",
            Self::SignInSecondFactor => "sign_in.second_factor",
            Self::SignInSecondFactorRefused => "sign_in.second_factor_refused",
            Self::SignInOidc => "sign_in.oidc",
            Self::SignOut => "sign_out",
            Self::SessionRevoked => "session.revoked",
            Self::SessionsRevokedAll => "sessions.revoked_all",
            Self::PasswordChanged => "password.changed",
            Self::TotpEnabled => "totp.enabled",
            Self::TotpDisabled => "totp.disabled",
            Self::EscrowReplaced => "escrow.replaced",
            Self::DeviceEnrolled => "device.enrolled",
            Self::ShareLinkCreated => "share_link.created",
            Self::ShareLinkRevoked => "share_link.revoked",
            Self::DropLinkCreated => "drop_link.created",
            Self::DropLinkRevoked => "drop_link.revoked",
        }
    }

    /// The kind `token` names, or `None` for a token this server does not write.
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == token)
    }
}

/// One entry in an account's activity log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEvent {
    /// This server's id for the event, a UUIDv7.
    pub event_id: String,
    /// The account whose log it is in.
    pub user_id: UserId,
    /// What happened.
    pub kind: ActivityKind,
    /// The server's clock when it was recorded.
    pub at: Timestamp,
    /// The request id of the request that caused it, when a request did.
    pub request_id: Option<String>,
    /// The session that acted, when one did. Absent for an unauthenticated act — a sign-in, a
    /// revoke-all redeemed with master-key proof.
    pub session_id: Option<SessionId>,
    /// The identifier the event is about: a session, a share link, a drop link.
    pub subject: Option<String>,
}

/// The activity log port.
pub trait ActivityStore: std::fmt::Debug + Send + Sync {
    /// Append `event` to its account's log, then delete that account's events recorded before
    /// `retain_from` and every event beyond the newest [`MAX_EVENTS_PER_ACCOUNT`] — one
    /// operation, so the log of an account that is recording never exceeds its bound.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Rejected`] if an event with the same `event_id` is already in the
    /// account's log. The id is a fresh UUIDv7 per event, so a collision is a bug, not a retry.
    fn record(&self, event: ActivityEvent, retain_from: Timestamp) -> StoreFuture<'_, ()>;

    /// `user`'s events recorded at or after `since`, newest first, at most `limit` of them.
    ///
    /// Newest first in the order they were **recorded**, not re-sorted by `at`: two events may
    /// share an instant, and the order a reader follows is the order the server wrote them.
    fn recent<'a>(
        &'a self,
        user: &'a UserId,
        since: Timestamp,
        limit: usize,
    ) -> StoreFuture<'a, Vec<ActivityEvent>>;
}

/// A deterministic in-memory adapter.
#[derive(Debug, Default)]
pub struct InMemoryActivity {
    /// Each account's log, oldest first.
    logs: Mutex<BTreeMap<UserId, Vec<ActivityEvent>>>,
}

impl InMemoryActivity {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ActivityStore for InMemoryActivity {
    fn record(&self, event: ActivityEvent, retain_from: Timestamp) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut logs = lock(&self.logs);
            let log = logs.entry(event.user_id.clone()).or_default();
            if log.iter().any(|held| held.event_id == event.event_id) {
                return Err(StoreError::Rejected {
                    store: "activity",
                    detail: format!("event {} is already recorded", event.event_id),
                });
            }
            log.push(event);
            log.retain(|held| held.at >= retain_from);
            let excess = log.len().saturating_sub(MAX_EVENTS_PER_ACCOUNT);
            log.drain(..excess);
            Ok(())
        })
    }

    fn recent<'a>(
        &'a self,
        user: &'a UserId,
        since: Timestamp,
        limit: usize,
    ) -> StoreFuture<'a, Vec<ActivityEvent>> {
        Box::pin(async move {
            Ok(lock(&self.logs)
                .get(user)
                .map(|log| {
                    log.iter()
                        .rev()
                        .filter(|event| event.at >= since)
                        .take(limit)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default())
        })
    }
}

/// The activity module's collaborators.
#[derive(Debug, Clone)]
pub struct ActivityContext {
    store: Arc<dyn ActivityStore>,
    clock: Arc<dyn Clock>,
    /// Writes [`Self::record_detached`] started and nobody has awaited, shared by every clone.
    detached: Arc<Mutex<tokio::task::JoinSet<()>>>,
    /// The open coalescing window of each account and detached kind, shared by every clone.
    windows: Arc<Mutex<Windows>>,
}

/// The coalescing windows [`ActivityContext::record_detached`] keeps.
#[derive(Debug, Default)]
struct Windows {
    /// When each account's window for a kind opened, and how many events it has absorbed.
    open: std::collections::HashMap<(UserId, ActivityKind), Window>,
    /// The size past which the next opening sweeps out closed windows; doubles with the live
    /// set, so the sweep's cost stays amortized constant per opening.
    sweep_at: usize,
}

/// One account's coalescing window for one kind.
#[derive(Debug, Clone, Copy)]
struct Window {
    /// The instant of the event that opened it, the one that was written.
    opened: Timestamp,
    /// The events counted into it and not written.
    coalesced: u64,
}

impl Window {
    /// Whether an event at `now` falls inside this window. A clock that stepped back is inside.
    fn holds(&self, now: Timestamp) -> bool {
        now.duration_since(self.opened) < DETACHED_COALESCE_WINDOW
    }
}

/// The fewest open windows that trigger a sweep.
const MIN_SWEEP_AT: usize = 1_024;

impl Windows {
    /// Count `user`'s `kind` at `now` into its open window, or open one.
    ///
    /// Returns `true` when the event opened a window and so must be written.
    fn admit(&mut self, user: &UserId, kind: ActivityKind, now: Timestamp) -> bool {
        let key = (user.clone(), kind);
        if let Some(window) = self.open.get_mut(&key)
            && window.holds(now)
        {
            window.coalesced += 1;
            tracing::debug!(
                user_id = %user,
                kind = kind.as_str(),
                coalesced = window.coalesced,
                opened = %window.opened,
                "coalesced an account activity event into its open window"
            );
            return false;
        }
        if let Some(closed) = self.open.insert(
            key,
            Window {
                opened: now,
                coalesced: 0,
            },
        ) && closed.coalesced > 0
        {
            tracing::info!(
                user_id = %user,
                kind = kind.as_str(),
                coalesced = closed.coalesced,
                opened = %closed.opened,
                "an account activity window closed with events coalesced into it"
            );
        }
        if self.open.len() > self.sweep_at.max(MIN_SWEEP_AT) {
            self.open.retain(|(user, kind), window| {
                let open = window.holds(now);
                if !open && window.coalesced > 0 {
                    tracing::info!(
                        user_id = %user,
                        kind = kind.as_str(),
                        coalesced = window.coalesced,
                        opened = %window.opened,
                        "an account activity window closed with events coalesced into it"
                    );
                }
                open
            });
            self.sweep_at = self.open.len() * 2;
            tracing::debug!(
                open = self.open.len(),
                "swept closed account activity windows"
            );
        }
        true
    }
}

impl ActivityContext {
    /// Assembles the module.
    pub fn new(store: Arc<dyn ActivityStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            clock,
            detached: Arc::default(),
            windows: Arc::default(),
        }
    }

    /// Where the log lives.
    pub fn store(&self) -> &dyn ActivityStore {
        self.store.as_ref()
    }

    /// The oldest instant a read may return at `now`.
    fn horizon(now: Timestamp) -> Timestamp {
        now.checked_sub(RETENTION).unwrap_or(Timestamp::MIN)
    }

    /// Record that `kind` happened to `user`, acted by `session` and about `subject`.
    ///
    /// Called **after** the action, and infallible by design: a store that cannot record logs
    /// the whole event at `error` — so the operator's log still holds it — and the action
    /// stands. The request id is the current request's, read from [`crate::request_span`].
    pub async fn record(
        &self,
        user: &UserId,
        kind: ActivityKind,
        session: Option<&SessionId>,
        subject: Option<&str>,
    ) {
        let event = self.stamp(user, kind, session, subject);
        write(self.store.as_ref(), event).await;
    }

    /// Record that `kind` happened to `user`, without the caller waiting for the write.
    ///
    /// For an event whose *presence* must not show in the response time: a refused or locked
    /// sign-in is recorded for an address that has an account and not for one that does not, so
    /// a write awaited on the response path would tell a caller which addresses exist. The
    /// event is stamped here — its instant and the current request's id — and only the store
    /// write runs on a task of its own, inside the current span so its log line still names the
    /// request. It is never awaited on any path, so a known address costs the caller the same as
    /// an unknown one, whatever the store does.
    ///
    /// It keeps fewer guarantees than [`Self::record`], because its events are caused by whoever
    /// knows an address rather than by the account's own sessions:
    ///
    /// - **Coalesced.** One event of a kind is written per account per
    ///   [`DETACHED_COALESCE_WINDOW`]; the rest are counted into it, each at `debug`, and the
    ///   count at `info` once the window is over and the next event or a sweep replaces it. The
    ///   request log still holds every attempt.
    /// - **Bounded.** Past [`MAX_DETACHED_IN_FLIGHT`] running writes, the event is dropped and
    ///   logged whole at `error`, as a refused write is.
    /// - **Drained, not durable, at exit.** A write still in flight when the process stops is
    ///   lost unless the shutdown awaits [`Self::settled`]; `serve` does, for a bounded time.
    pub fn record_detached(&self, user: &UserId, kind: ActivityKind) -> Detached {
        use tracing::Instrument as _;

        let mut detached = lock(&self.detached);
        // Reap what has finished, so the set holds only writes in flight.
        while let Some(finished) = detached.try_join_next() {
            if let Err(error) = finished {
                tracing::error!(%error, "a detached activity write did not finish");
            }
        }
        let event = self.stamp(user, kind, None, None);
        let mut windows = lock(&self.windows);
        if !windows.admit(user, kind, event.at) {
            return Detached::Coalesced;
        }
        if detached.len() >= MAX_DETACHED_IN_FLIGHT {
            // Nothing was written for this window, so the next attempt opens it again.
            windows.open.remove(&(user.clone(), kind));
            tracing::error!(
                user_id = %event.user_id,
                kind = kind.as_str(),
                event_id = event.event_id,
                at = %event.at,
                request_id = ?event.request_id,
                in_flight = detached.len(),
                "an account activity event was dropped: too many detached writes in flight"
            );
            return Detached::Dropped;
        }
        drop(windows);
        let store = Arc::clone(&self.store);
        detached.spawn(
            async move { write(store.as_ref(), event).await }.instrument(tracing::Span::current()),
        );
        tracing::trace!(
            user_id = %user,
            kind = kind.as_str(),
            in_flight = detached.len(),
            "detached an account activity write"
        );
        Detached::Started
    }

    /// Wait until every write [`Self::record_detached`] started before this call has finished.
    ///
    /// What a test reads the log after, and what a shutdown that wants no event lost awaits.
    pub async fn settled(&self) {
        let mut pending = std::mem::take(&mut *lock(&self.detached));
        while let Some(finished) = pending.join_next().await {
            if let Err(error) = finished {
                tracing::error!(%error, "a detached activity write did not finish");
            }
        }
    }

    /// The event `kind` happening to `user` now, in the current request.
    fn stamp(
        &self,
        user: &UserId,
        kind: ActivityKind,
        session: Option<&SessionId>,
        subject: Option<&str>,
    ) -> ActivityEvent {
        ActivityEvent {
            event_id: uuid::Uuid::now_v7().to_string(),
            user_id: user.clone(),
            kind,
            at: self.clock.now(),
            request_id: crate::request_span::current_request_id().map(|id| id.to_string()),
            session_id: session.cloned(),
            subject: subject.map(str::to_owned),
        }
    }

    /// `user`'s most recent events inside the retention window, newest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// The store's own error, for the route to answer as an outage.
    pub async fn recent(
        &self,
        user: &UserId,
        limit: usize,
    ) -> Result<Vec<ActivityEvent>, StoreError> {
        let since = Self::horizon(self.clock.now());
        self.store
            .recent(user, since, limit.min(MAX_EVENTS_PER_ACCOUNT))
            .await
    }
}

/// Append `event` to its account's log, logging it at `error` when the store refuses.
///
/// Infallible for the reason [`ActivityContext::record`] gives: the action has already happened.
async fn write(store: &dyn ActivityStore, event: ActivityEvent) {
    // The fields the log line names, kept before the event moves into the store.
    let kind = event.kind;
    let event_id = event.event_id.clone();
    let user_id = event.user_id.clone();
    let session_id = event.session_id.clone();
    let subject = event.subject.clone();
    let horizon = ActivityContext::horizon(event.at);
    match store.record(event, horizon).await {
        Ok(()) => tracing::info!(
            user_id = %user_id,
            kind = kind.as_str(),
            event_id,
            session_id = ?session_id,
            subject,
            "recorded an account activity event"
        ),
        Err(error) => tracing::error!(
            %error,
            user_id = %user_id,
            kind = kind.as_str(),
            event_id,
            session_id = ?session_id,
            subject,
            "an account activity event could not be recorded; the action stands"
        ),
    }
}

pub mod conformance;
pub mod postgres;

pub use postgres::PostgresActivity;

#[cfg(test)]
mod tests;
