//! Storage quota accounting and enforcement (`S-C6`).
//!
//! # Attribution, and why dedup makes it interesting
//!
//! Bytes are attributed to the **uploader**, not the owner: a user may upload on behalf of
//! somebody else with verified permission, and the storage cost belongs to whoever spent it.
//!
//! Content-addressed dedup then forces a rule that is easy to get backwards. A blob shared
//! between two uploaders counts against **only the first** — the second is a merge, not a
//! second copy, and there is nothing to charge for. That is not a courtesy: without it a
//! malicious user could exhaust another account's quota by re-uploading blobs whose addresses
//! they already know. So attribution is keyed on the **content address**, globally, and
//! [`QuotaStore::charge`] answers "already attributed" rather than debiting twice.
//!
//! # One hard enforcement point
//!
//! Session creation, and nothing else. Once a session is open, the declared size is the cap and
//! the session is allowed to finish — a client that has already sent 900MB of a 1GB blob must
//! not be refused at finalization, because the bytes are already on the server and refusing
//! costs storage rather than saving it. Cancellation and expiry release the reservation; a
//! metadata-update is checked as a growth write rather than as an upload.
//!
//! # What each state refuses, and what it never refuses
//!
//! | State | Uploads | Metadata growth | Delete / restore |
//! | --- | --- | --- | --- |
//! | [`QuotaState::Ok`], [`QuotaState::SoftWarning`] | admitted | admitted | admitted |
//! | [`QuotaState::HardExceeded`] | refused | admitted | admitted |
//! | [`QuotaState::GraceExpired`] | refused | refused | **admitted** |
//!
//! That last cell is the one the design is emphatic about: the provenance and metadata writes a
//! `delete`, a `trash-restore` or a trash-empty produces are *always* admitted, because a user
//! must be able to delete their way back under quota. A quota that could lock someone out of
//! freeing space would be a trap rather than a limit.
//!
//! # How an account reaches [`QuotaState::HardExceeded`] at all
//!
//! Not through an upload. The enforcement point refuses on the *projected* total — current use
//! plus the declared size — so a session that would cross the limit never opens, and one that
//! opens leaves the account under it. Being over is therefore reached by a **lowered limit**, or
//! by growth the session check did not project: a metadata blob, a derivative, a federated
//! receive.
//!
//! That is a consequence of the design rather than a gap in it, and it is worth stating because
//! it is easy to write a test that assumes an upload can push an account over and then to
//! "fix" the enforcement when the test fails. The states exist to describe an account that *is*
//! over, however it got there.
//!
//! # A self-hosted server has no quota, and that is the default
//!
//! [`QuotaLimits::unlimited`] is what a deployment with no billing runs, and it is what the
//! server does unless configured otherwise. Every predicate here is written so that an
//! unlimited deployment takes the same code path as a limited one rather than a special case —
//! `hard_limit = u64::MAX` is never crossed, so the state is always [`QuotaState::Ok`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};

use crate::blob::ContentAddress;
use crate::store::{StoreFuture, UserId};

/// The default grace window: how long a user may sit over the hard limit before metadata-growth
/// writes are refused too.
pub const DEFAULT_GRACE_WINDOW: SignedDuration = SignedDuration::from_hours(24 * 14);

/// A deployment's thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaLimits {
    /// Where the warning starts.
    pub soft_limit: u64,
    /// Where uploads stop.
    pub hard_limit: u64,
    /// How long over the hard limit before metadata growth stops too.
    pub grace_window: SignedDuration,
}

impl QuotaLimits {
    /// A deployment with no quota — the self-hosted default.
    ///
    /// Not a bypass: `u64::MAX` is never crossed, so an unlimited server runs the same
    /// predicates a limited one does. A separate "quota off" branch is a branch nothing tests.
    pub fn unlimited() -> Self {
        Self {
            soft_limit: u64::MAX,
            hard_limit: u64::MAX,
            grace_window: DEFAULT_GRACE_WINDOW,
        }
    }

    /// A deployment with the given limits.
    pub fn new(soft_limit: u64, hard_limit: u64, grace_window: SignedDuration) -> Self {
        Self {
            soft_limit,
            hard_limit,
            grace_window,
        }
    }
}

impl Default for QuotaLimits {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Which account state a user is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuotaState {
    /// Under the soft limit.
    Ok,
    /// Over the soft limit, under the hard one. Uploads still succeed; the client warns.
    SoftWarning,
    /// At or over the hard limit. New uploads are refused; every other write still works.
    HardExceeded,
    /// Over the hard limit for longer than the grace window. Metadata growth is refused too.
    GraceExpired,
}

impl QuotaState {
    /// The stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::SoftWarning => "soft_warning",
            Self::HardExceeded => "hard_exceeded",
            Self::GraceExpired => "grace_expired",
        }
    }
}

/// What kind of write is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteClass {
    /// An upload session. The only hard enforcement point.
    Upload,
    /// A write that grows stored metadata — a new metadata blob, a share link.
    MetadataGrowth,
    /// A write that cannot increase storage, or that exists to decrease it.
    ///
    /// Always admitted, whatever the state. A user must be able to delete their way back under
    /// quota, and the provenance record a delete produces is a write.
    Lifecycle,
}

/// What the store holds for one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoredUsage {
    /// Bytes attributed to them.
    pub used: u64,
    /// When they first crossed the hard limit and have not been under it since, if ever.
    ///
    /// Kept by the store rather than derived, because "how long have you been over" cannot be
    /// computed from a current total.
    pub over_since: Option<Timestamp>,
}

/// What charging an address did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeOutcome {
    /// The bytes were attributed to this user.
    Charged {
        /// Their total after the charge.
        used: u64,
    },
    /// The address is already attributed — to this user or to another. Nothing was debited.
    ///
    /// One value for both, deliberately. Telling a caller "somebody else already holds these
    /// bytes" would answer, from a quota endpoint, the cross-tenant question
    /// [`crate::index::AssetIndex::find_by_address`] is owner-scoped to avoid.
    AlreadyAttributed,
}

/// Where quota accounting lives.
pub trait QuotaStore: std::fmt::Debug + Send + Sync {
    /// What `user` currently owes.
    fn usage<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, StoredUsage>;

    /// Attribute `size` bytes at `address` to `user`, unless the address is already attributed.
    ///
    /// The check and the debit are one operation: two concurrent sessions for the same address
    /// that both read "unattributed" and then both debited would charge twice for one blob.
    fn charge<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome>;

    /// Release the attribution for `address`, if it is `user`'s.
    ///
    /// `true` when something was released. Called when a session is cancelled or expires — the
    /// bytes were reserved and never arrived — and when a blob is hard-purged.
    fn release<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, bool>;

    /// Release `address` to whoever the ledger holds it against, returning them and the bytes.
    ///
    /// The collector's release (`S-C44`), and it is a **separate operation** rather than a
    /// nullable-user variant of [`QuotaStore::release`] because the two want opposite things
    /// from the same word. Cancellation is one account undoing its own reservation, and
    /// refusing another account's attribution there is the point. A sweep knows an address and
    /// nothing else: attribution is global by content address, so the blob it is deleting may be
    /// charged to an account with no remaining connection to the asset whose purge exposed it.
    /// The collector cannot supply the user and must not guess one, so it asks the ledger.
    ///
    /// `None` when the address was not attributed — which is the ordinary case for a blob the
    /// ledger never saw, and is not an error.
    fn release_attribution<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, Option<(UserId, u64)>>;
}

/// The state `used` puts a user in, given how long they have been over.
///
/// Pure, so the state machine is testable without a store and the same function decides it for
/// every adapter.
pub fn state_of(
    used: u64,
    over_since: Option<Timestamp>,
    now: Timestamp,
    limits: QuotaLimits,
) -> QuotaState {
    if used < limits.soft_limit {
        return QuotaState::Ok;
    }
    if used < limits.hard_limit {
        return QuotaState::SoftWarning;
    }
    match over_since {
        Some(since) if now.duration_since(since) > limits.grace_window => QuotaState::GraceExpired,
        // Inside the window, or over the limit with no recorded crossing. The second is treated
        // as newly over rather than as expired: the refusing direction there would lock a user
        // out of metadata growth on the strength of a missing timestamp.
        _ => QuotaState::HardExceeded,
    }
}

/// Whether a write of `class` costing `additional` bytes may proceed.
///
/// The single decision every enforcement point makes, so "where is quota checked" has one
/// answer per call site and not one answer per surface.
pub fn admits(
    state: QuotaState,
    class: WriteClass,
    used: u64,
    additional: u64,
    limits: QuotaLimits,
) -> bool {
    match class {
        // A user must always be able to delete their way back under quota.
        WriteClass::Lifecycle => true,
        WriteClass::MetadataGrowth => state != QuotaState::GraceExpired,
        // Checked against the projected total, not the current one: the point of enforcing at
        // session creation is that the declared size becomes the cap for the whole transfer.
        WriteClass::Upload => used.saturating_add(additional) < limits.hard_limit,
    }
}

/// A deterministic in-memory adapter.
///
/// One mutex over both maps, which is what makes [`Self::charge`]'s check-and-debit atomic.
#[derive(Debug, Default)]
pub struct InMemoryQuota {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Who each address's bytes are charged to, and how many.
    attributed: BTreeMap<ContentAddress, (UserId, u64)>,
    /// Each user's total and the moment they went over.
    usage: BTreeMap<UserId, StoredUsage>,
}

impl Inner {
    /// Give `size` bytes back to `user`.
    ///
    /// One place, because both releases do exactly this and a second copy would eventually
    /// forget the `over_since` half — which would leave an account that dropped back under its
    /// limit still carrying the clock that decides when a soft limit becomes a hard one.
    fn credit(&mut self, user: &UserId, size: u64) {
        if let Some(entry) = self.usage.get_mut(user) {
            entry.used = entry.used.saturating_sub(size);
            // Back under the limit: the clock stops, so a later crossing gets a fresh window
            // rather than inheriting an expired one.
            entry.over_since = None;
        }
    }
}

impl InMemoryQuota {
    /// An empty ledger.
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

impl QuotaStore for InMemoryQuota {
    fn usage<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, StoredUsage> {
        Box::pin(async move {
            Ok(lock(&self.inner)
                .usage
                .get(user)
                .copied()
                .unwrap_or_default())
        })
    }

    fn charge<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            if inner.attributed.contains_key(address) {
                return Ok(ChargeOutcome::AlreadyAttributed);
            }
            inner
                .attributed
                .insert(address.clone(), (user.clone(), size));
            let entry = inner.usage.entry(user.clone()).or_default();
            entry.used = entry.used.saturating_add(size);
            // The moment they crossed, recorded on the crossing rather than on every write, so
            // the grace window measures from when it started.
            if entry.used >= limits.hard_limit && entry.over_since.is_none() {
                entry.over_since = Some(at);
                tracing::info!(%user, used = entry.used, "an account crossed its hard quota limit");
            }
            Ok(ChargeOutcome::Charged { used: entry.used })
        })
    }

    fn release<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some((owner, size)) = inner.attributed.get(address).cloned() else {
                return Ok(false);
            };
            if &owner != user {
                return Ok(false);
            }
            inner.attributed.remove(address);
            inner.credit(user, size);
            Ok(true)
        })
    }

    fn release_attribution<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, Option<(UserId, u64)>> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some((owner, size)) = inner.attributed.remove(address) else {
                return Ok(None);
            };
            inner.credit(&owner, size);
            tracing::info!(
                user = %owner,
                %address,
                size,
                "a swept blob's bytes were credited back to the account they were charged to"
            );
            Ok(Some((owner, size)))
        })
    }
}

/// The quota module's collaborators.
#[derive(Debug, Clone)]
pub struct QuotaContext {
    quotas: Arc<dyn QuotaStore>,
    clock: Arc<dyn crate::store::Clock>,
    limits: QuotaLimits,
}

impl QuotaContext {
    /// Assembles the module from its collaborators and the deployment's limits.
    pub fn new(
        quotas: Arc<dyn QuotaStore>,
        clock: Arc<dyn crate::store::Clock>,
        limits: QuotaLimits,
    ) -> Self {
        Self {
            quotas,
            clock,
            limits,
        }
    }

    /// The ledger.
    pub fn quotas(&self) -> &dyn QuotaStore {
        self.quotas.as_ref()
    }

    /// The clock a crossing is stamped from.
    pub fn clock(&self) -> &dyn crate::store::Clock {
        self.clock.as_ref()
    }

    /// The deployment's thresholds.
    pub fn limits(&self) -> QuotaLimits {
        self.limits
    }
}

/// Charge an upload's declared size, refusing and compensating if it crosses the hard limit.
///
/// **Charge first, then check** — which looks backwards and is not. Content-addressed dedup
/// means a blob another account already holds costs this one nothing, and a check taken before
/// the charge cannot know that: it would refuse an upload that would have added zero bytes,
/// which is the wrong direction for a limit whose whole point is measuring real storage. So the
/// ledger decides whether anything was actually added, and a charge that turns out to cross the
/// limit is released again. The store's charge is atomic, so the compensating release is
/// undoing this caller's own debit and nobody else's.
///
/// # Errors
///
/// Propagates the ledger's failure. Nothing is charged when it does.
pub async fn charge_upload(
    context: &QuotaContext,
    user: &UserId,
    address: &ContentAddress,
    size: u64,
) -> Result<UploadCharge, crate::store::StoreError> {
    let limits = context.limits();
    let outcome = context
        .quotas()
        .charge(user, address, size, context.clock().now(), limits)
        .await?;
    let ChargeOutcome::Charged { used } = outcome else {
        // Somebody already holds these bytes. A merge is not storage, so there is nothing to
        // refuse and nothing to release.
        return Ok(UploadCharge::Admitted);
    };
    if used < limits.hard_limit {
        return Ok(UploadCharge::Admitted);
    }
    context.quotas().release(user, address).await?;
    tracing::info!(%user, used, "an upload was refused: it would cross the hard quota limit");
    Ok(UploadCharge::Refused)
}

/// What charging an upload decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadCharge {
    /// The session may open. Either the bytes were charged, or they were already somebody's.
    Admitted,
    /// It would cross the hard limit. Nothing is charged — the debit was released.
    Refused,
}

/// The state `user` is in right now.
///
/// # Errors
///
/// Propagates the ledger's failure.
pub async fn current_state(
    context: &QuotaContext,
    user: &UserId,
) -> Result<QuotaState, crate::store::StoreError> {
    let usage = context.quotas().usage(user).await?;
    Ok(state_of(
        usage.used,
        usage.over_since,
        context.clock().now(),
        context.limits(),
    ))
}

pub mod conformance;
pub mod postgres;

pub use self::postgres::PostgresQuota;

#[cfg(test)]
mod tests;
