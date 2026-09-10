//! Moderation (`S-C8`) — account standing, and the record a user can read about it.
//!
//! # Why this has almost no wire surface
//!
//! Moderation *actions* are operator actions, and this crate already has a shape for those:
//! [`crate::gc`] and [`crate::scrub`] are ports with no HTTP surface at all, driven by one-shot
//! operator binaries. Suspending an account is the same kind of thing, and for a stronger
//! reason — design/moderation.md names an **admin queue** and an admin who acts on it, and
//! specifies no way for an admin to authenticate. Inventing one would be inventing the most
//! sensitive authentication surface on the server from nothing, so the actions live behind the
//! port and the decision is recorded rather than guessed.
//!
//! What *is* on the wire is the half the contract makes user-facing:
//!
//! - a suspended account's upload session creation is refused (`error.moderation.account_suspended`),
//! - and the user reads the record of what was done to them.
//!
//! # No silent operations, and what that costs
//!
//! design/moderation.md's structural rule is that a user whose asset stops serving is never left
//! to guess why. So every action here **writes an event and applies the effect in one
//! operation** ([`ModerationStore::apply`]) rather than leaving the log to a caller who might
//! forget: a takedown that failed to record itself is exactly the silent operation the rule
//! forbids, and it would fail silently in the direction that hides it.
//!
//! # The federated half (`S-C49`)
//!
//! Both halves design/moderation.md names are now here. **Federated report intake** is
//! [`ModerationStore::file_report`], written by `POST /v1/federation/reports` once the report's
//! Ed25519 signature verifies against the reporting peer's operator-pinned key and the
//! `(reporting_server, reported_user)` budget admits it; [`ModerationStore::pending_reports`] is
//! how an operator reads the queue. The content is a hash and an album pointer and nothing else,
//! because a report must not become a channel for a peer to say things about a user.
//!
//! **The server-level blocklist** is not here and is not meant to be: it operates at the
//! federation-capability layer, so it is a column on [`crate::federation::PeerRecord`] and is
//! consulted at mint, at every presentation, at refresh and at intake. Per-user blocks are
//! MLS-side and never propagate.
//!
//! # What is still not here, and why
//!
//! **An admin surface.** design/moderation.md names an admin queue and an admin who acts on it,
//! and specifies no way for that admin to authenticate. [`ModerationStore::pending_reports`] is
//! the queue; reading it over HTTP is what waits for an admin authentication model.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;

use crate::store::{AlbumId, AssetId, StoreFuture, UserId};

/// Whether an account may act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// The ordinary state.
    Active,
    /// Suspended by an admin.
    ///
    /// **Access-level, never data-level.** design/moderation.md is explicit that the user's data
    /// is untouched: a suspension removes the ability to upload and to share, and deliberately
    /// **not** the ability to sign out everywhere — `revoke_all_sessions` is gated by master-key
    /// proof rather than by account standing, and a suspended user whose account may also be
    /// compromised needs it most.
    Suspended {
        /// When it began.
        since: Timestamp,
    },
}

impl Standing {
    /// Whether this standing permits writing new content.
    pub fn may_write(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// What an admin did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationAction {
    /// The account was suspended.
    Suspended,
    /// A suspension was lifted.
    Reinstated,
    /// An asset was made unservable.
    TakenDown,
    /// An asset was placed under a legal hold.
    LegalHold,
    /// A hold on an asset was lifted.
    HoldLifted,
}

impl ModerationAction {
    /// The name this action travels under, on the wire and in a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Suspended => "suspended",
            Self::Reinstated => "reinstated",
            Self::TakenDown => "taken_down",
            Self::LegalHold => "legal_hold",
            Self::HoldLifted => "hold_lifted",
        }
    }
}

/// One entry in an account's moderation record.
///
/// The user reads these. That is the point — *"a user whose asset stops serving is never left to
/// guess why"* — so the fields are chosen for a person reading their own audit log, not for an
/// admin console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationEvent {
    /// The account the action was taken against.
    pub user_id: UserId,
    /// What was done.
    pub action: ModerationAction,
    /// The asset, when the action was about one rather than about the account.
    pub asset_id: Option<AssetId>,
    /// When it happened.
    pub at: Timestamp,
    /// Why, *where policy permits*.
    ///
    /// Optional because the contract says "where policy permits", not "always": a legal hold may
    /// come with an obligation not to disclose it. Absent is a real answer here and reads to the
    /// user as "we are not able to say", which is honest — where a fabricated reason would not
    /// be.
    pub reason: Option<String>,
}

/// A moderation report one peer server filed against an account on this one (`S-C49`).
///
/// # The content is a pointer, not a complaint
///
/// design/moderation.md fixes what a federated report may carry: the reported user, the asset's
/// **content hash** and the album it is in, and a short reason. No text about the person, no
/// evidence blob, no copy of anything. A report is a request that this server's operator look at
/// something it already holds — everything else would make the intake a channel for a peer to
/// publish claims about a user into this server's storage.
///
/// # Re-verifiable, which means the *signed bytes* are what is kept
///
/// The signature is kept so an operator can re-verify it long after the fact, and so a key
/// rotation cannot silently turn an accepted report into an unattributable one. That is only
/// true if what is stored is what was signed — and the fields below are not: `reporting_server`
/// is the canonical [`PeerId`](crate::federation::PeerId) form (case-folded, trailing dot
/// stripped) rather than the string the peer sent, and `reported_at` is a parsed instant rather
/// than the RFC 3339 text. Re-encoding those back into a claim would produce different bytes and
/// a signature that no longer verifies.
///
/// So [`FederatedReport::signed`] holds the exact canonical-CBOR bytes the signature covers, and
/// every field below is **derived from them** at intake rather than assembled beside them. An
/// operator re-verifies with `signed` and the peer's pinned key and needs nothing else;
/// `moderation::tests` pins the round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederatedReport {
    /// This server's identifier for the report, a UUIDv7.
    pub report_id: String,
    /// The peer that filed it, as its own `server-info` names it.
    pub reporting_server: String,
    /// The account on **this** server the report is about.
    pub reported_user: UserId,
    /// The content address of the asset complained about.
    pub asset_hash: String,
    /// The album it was pulled from.
    pub album_id: AlbumId,
    /// The peer's short reason, where it gave one.
    pub reason: Option<String>,
    /// When the peer says it was reported.
    pub reported_at: Timestamp,
    /// When this server accepted it. The only timestamp this server vouches for.
    pub received_at: Timestamp,
    /// The peer's Ed25519 signature over [`Self::signed`].
    pub signature: Vec<u8>,
    /// The exact canonical-CBOR bytes the signature covers.
    ///
    /// Stored verbatim, never rebuilt: every other field on this record is derived from these
    /// bytes, and re-encoding a normalized field would produce a report nobody can attribute.
    pub signed: Vec<u8>,
}

/// The account-standing and moderation-record port.
pub trait ModerationStore: std::fmt::Debug + Send + Sync {
    /// Apply `event` and move `standing` to match, as one operation.
    ///
    /// The two together, never separately. A takedown that applied and failed to record itself
    /// is the silent operation the contract forbids, and a record with no effect is worse: it
    /// tells a user something happened that did not.
    fn apply(&self, event: ModerationEvent, standing: Option<Standing>) -> StoreFuture<'_, ()>;

    /// `user`'s current standing. [`Standing::Active`] for an account nothing has been done to.
    fn standing<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Standing>;

    /// Everything done to `user`, oldest first.
    ///
    /// The order is part of the contract: this is a user-visible surface, and a reader following
    /// what happened to their account needs it in the order it happened.
    fn events_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<ModerationEvent>>;

    /// Record a federated report (`S-C49`).
    ///
    /// Writes nothing about the reported account's standing: a peer's report is an *input* to a
    /// decision, never a decision. Filing one has no effect a user can observe, which is why it
    /// is not a [`ModerationEvent`] — the no-silent-operations rule is about actions taken
    /// against a user, and nothing has been taken.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Rejected`](crate::store::StoreError::Rejected) if a report with the
    /// same `report_id` is already recorded. The id is a fresh UUIDv7 per accepted report, so a
    /// collision is a bug rather than a retry.
    fn file_report(&self, report: FederatedReport) -> StoreFuture<'_, ()>;

    /// Every federated report on file, oldest first.
    ///
    /// "Pending" is the whole set until an admin surface exists to work through it — there is no
    /// authentication model for the admin who would resolve one (see the module docs), so a
    /// resolved state would be a column nothing could ever set.
    fn pending_reports(&self) -> StoreFuture<'_, Vec<FederatedReport>>;
}

/// The most federated reports the in-memory adapter keeps.
///
/// Reports arrive on an unauthenticated route from parties an operator pinned, and an in-memory
/// map with no eviction is process memory that never returns — a `--memory` deployment left
/// running would grow until it did not. Ten thousand is far above any real queue an operator
/// works by hand and far below anything that matters to a process.
///
/// **Eviction is oldest-first and loud.** A dropped report is a moderation input nobody will
/// ever see, so it is a `warn`, not a silent trim; the durable adapter (#476) is where a queue
/// that must not lose anything belongs.
pub const MAX_IN_MEMORY_REPORTS: usize = 10_000;

/// A deterministic in-memory adapter.
#[derive(Debug, Default)]
pub struct InMemoryModeration {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    standing: BTreeMap<UserId, Standing>,
    events: BTreeMap<UserId, Vec<ModerationEvent>>,
    /// Keyed by `report_id`, which is a UUIDv7 — so iteration order is arrival order.
    reports: BTreeMap<String, FederatedReport>,
}

impl InMemoryModeration {
    /// An empty store: every account active, nothing on record.
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

impl ModerationStore for InMemoryModeration {
    fn apply(&self, event: ModerationEvent, standing: Option<Standing>) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let user = event.user_id.clone();
            tracing::info!(
                %user,
                action = event.action.as_str(),
                asset = ?event.asset_id,
                "a moderation action was recorded"
            );
            if let Some(standing) = standing {
                inner.standing.insert(user.clone(), standing);
            }
            inner.events.entry(user).or_default().push(event);
            Ok(())
        })
    }

    fn standing<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Standing> {
        Box::pin(async move {
            Ok(lock(&self.inner)
                .standing
                .get(user)
                .cloned()
                .unwrap_or(Standing::Active))
        })
    }

    fn file_report(&self, report: FederatedReport) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            if inner.reports.contains_key(&report.report_id) {
                return Err(crate::store::StoreError::Rejected {
                    store: "moderation",
                    detail: format!("report {} is already on file", report.report_id),
                });
            }
            tracing::info!(
                report = %report.report_id,
                from = %report.reporting_server,
                about = %report.reported_user,
                album = %report.album_id,
                "a federated moderation report was filed"
            );
            inner.reports.insert(report.report_id.clone(), report);
            // The `report_id` is a UUIDv7, so the map's own order is arrival order and the first
            // key is the oldest report.
            while inner.reports.len() > MAX_IN_MEMORY_REPORTS {
                let Some(oldest) = inner.reports.keys().next().cloned() else {
                    break;
                };
                tracing::warn!(
                    report = %oldest,
                    kept = MAX_IN_MEMORY_REPORTS,
                    "the in-memory report queue is full; the oldest report was dropped"
                );
                inner.reports.remove(&oldest);
            }
            Ok(())
        })
    }

    fn pending_reports(&self) -> StoreFuture<'_, Vec<FederatedReport>> {
        Box::pin(async move { Ok(lock(&self.inner).reports.values().cloned().collect()) })
    }

    fn events_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<ModerationEvent>> {
        Box::pin(async move {
            Ok(lock(&self.inner)
                .events
                .get(user)
                .cloned()
                .unwrap_or_default())
        })
    }
}

/// The moderation module's collaborators.
#[derive(Debug, Clone)]
pub struct ModerationContext {
    store: Arc<dyn ModerationStore>,
}

impl ModerationContext {
    /// Assembles the module.
    pub fn new(store: Arc<dyn ModerationStore>) -> Self {
        Self { store }
    }

    /// Where standing and the record live.
    pub fn store(&self) -> &dyn ModerationStore {
        self.store.as_ref()
    }
}

#[cfg(test)]
mod tests;
