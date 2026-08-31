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
//! # What is not here, and why
//!
//! - **Federated report intake** needs a peer's signing key to verify against, and federation
//!   has no surface on this port. Its rate limit needs `S-C32`'s counter besides.
//! - **The server-level blocklist** operates at the federation-capability layer, which likewise
//!   does not exist here.
//!
//! Both are `S-C8` deliverables and both are recorded as owed rather than stubbed, because a
//! blocklist nothing consults is worse than an absent one: it reads as protection.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;

use crate::store::{AssetId, StoreFuture, UserId};

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
}

/// A deterministic in-memory adapter.
#[derive(Debug, Default)]
pub struct InMemoryModeration {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    standing: BTreeMap<UserId, Standing>,
    events: BTreeMap<UserId, Vec<ModerationEvent>>,
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
