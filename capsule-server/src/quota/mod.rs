//! Storage quota accounting and enforcement.
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
//! Session creation, and the dedup re-charge a release runs. Once a session is open and
//! charged, the declared size is the cap and the session is allowed to finish — a client that
//! has already sent 900MB of a 1GB blob must not be refused at finalization, because the bytes
//! are already on the server and refusing costs storage rather than saving it. Cancellation and
//! expiry release the reservation, and a session another account's charge admitted is then
//! checked for itself (see below); a metadata-update is checked as a growth write rather than
//! as an upload.
//!
//! # What each state refuses, and what it never refuses
//!
//! | State | Uploads | Metadata growth | Delete / restore |
//! | --- | --- | --- | --- |
//! | [`QuotaState::Ok`], [`QuotaState::SoftWarning`] | admitted | admitted | admitted |
//! | [`QuotaState::HardExceeded`] | refused | admitted | admitted |
//! | [`QuotaState::GraceExpired`] | refused | refused | **admitted** |
//! | [`QuotaState::Suspended`] | refused | refused | **admitted** |
//!
//! That last column is the one the design is emphatic about: the provenance and metadata writes a
//! `delete`, a `trash-restore` or a trash-empty produces are *always* admitted, because a user
//! must be able to delete their way back under quota. A quota that could lock someone out of
//! freeing space would be a trap rather than a limit.
//!
//! # Suspension is an operator's act, and it outranks the thresholds
//!
//! [`QuotaState::Suspended`] is not reached by bytes. An operator sets it (`capsule-server quota
//! suspend`) for an administrative or billing reason, and only an operator clears it. While it is
//! set it is the state [`classify`] reports whatever the account's usage, and it refuses exactly
//! what [`QuotaState::GraceExpired`] refuses: every growth write, never a read and never a
//! lifecycle write. It is a quota state and not a moderation standing — a moderation suspension
//! (`crate::moderation`) removes the ability to write at all, which is a different act with a
//! different refusal code.
//!
//! The instant it was set is stored rather than a flag, so an operator reading the ledger after
//! an incident can say when, and so suspending an account twice keeps the first instant.
//!
//! # How an account reaches [`QuotaState::HardExceeded`] at all
//!
//! Not through an upload. The enforcement point refuses on the *projected* total — current use
//! plus the declared size — so a session that would cross the limit never opens, and one that
//! opens leaves the account under it. Being over is therefore reached by a **lowered limit**, or
//! by growth the session check did not project: a metadata blob, a derivative, a federated
//! receive.
//!
//! One upload path does reach it, narrowly. A session admitted `AlreadyAttributed` against
//! another account's charge is charged for itself when that account releases the attribution
//! ([`crate::upload::reservation`]), through [`charge_upload`], and a refusal discards the
//! session rather than letting it store uncharged bytes. A session whose finalization is
//! already claimed cannot be discarded — its bytes are all on the server and its finalizer owns
//! its terminal write — so it alone is charged past the hard limit and a suspension.
//!
//! That is a consequence of the design rather than a gap in it, and it is worth stating because
//! it is easy to write a test that assumes an upload can push an account over and then to
//! "fix" the enforcement when the test fails. The states exist to describe an account that *is*
//! over, however it got there.
//!
//! # A reservation is an attribution no committed blob has settled yet (#1059)
//!
//! An upload session is charged when it is created, before a byte arrives, and that charge is a
//! **reservation** ([`QuotaStore::reserve`]): it carries a deadline, `created_at` plus the
//! session lifetime cap ([`crate::store::upload::LIFETIME_CAP`]). A finalization that commits
//! the bytes **settles** it ([`QuotaStore::settle`]), and from then on it is an ordinary charge
//! the collector releases with the blob. Every other charge — a share's metadata record, a
//! rebuild's re-charge of stored bytes — pays for bytes already stored and is settled from the
//! start ([`QuotaStore::charge`]).
//!
//! A session that ends without its bytes gives its reservation back on the path that ended it
//! ([`crate::upload::reservation::release`]). One that simply expires has no such path: the
//! session store drops the record and nothing observes it. The reservation sweep
//! ([`crate::upload::reservation::ReservationSweep`]) closes that gap by walking the
//! reservations past their deadline ([`QuotaStore::expired_reservations`]), bounded by the
//! deadline index rather than by every stored blob, and releasing each one no committed blob
//! and no live session holds ([`QuotaStore::release_expired`]). The release is conditional on
//! the row still being an expired reservation, so a settle or an extension that lands first
//! wins and a second sweep releases nothing.
//!
//! # Pulled blobs count against every receiver, once each (#1162)
//!
//! A metadata blob a federation pull holds (`crate::federation::pull::pulled`) is charged to
//! each local account subscribed to an album that holds it, live or lapsed, **in full** and
//! **once per account** however many of its albums, on however many peers, hold it. That is
//! a second rule beside the global one above, and it is per receiver on purpose: a pulled album
//! is shared by its subscribers, so no subscriber is "first", and charging only one of them
//! would let the others hold the bytes free.
//!
//! The pulled charge is **derived, never stored**: [`StoredUsage::used`] is the ledger's
//! attributions plus the distinct pulled blobs [`PulledHoldings`] says the account holds now.
//! So the charge lands with the pulled page that holds the blob (the rows are written in the
//! page's own commit) and is released by whatever ends the holding — an unsubscribe, or the
//! job's rows being forgotten — with no second write that could run twice, run never, or credit
//! below zero. A blob the account itself is attributed in the ledger (an upload, a reservation)
//! is counted there and not again as pulled; one another account is attributed is still charged
//! to the receiver, because dropping it would make `GET /v1/quota` say that somebody on this
//! server uploaded those bytes — the cross-tenant answer [`ChargeOutcome::AlreadyAttributed`]
//! refuses to give.
//!
//! Pulled growth is never refused here — it is bounded by the per-peer caching budget
//! ([`crate::federation::pull::pulled::PeerCachingBudget`]), which caps the same bytes rather
//! than adding to them — so it is one of the ways an account becomes
//! [`QuotaState::HardExceeded`], and unsubscribing is always the way back.
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
    /// Suspended by an operator (an administrative or billing act), whatever the usage.
    ///
    /// Refuses every growth write, as [`Self::GraceExpired`] does; reads and lifecycle writes are
    /// still admitted.
    Suspended,
}

impl QuotaState {
    /// The stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::SoftWarning => "soft_warning",
            Self::HardExceeded => "hard_exceeded",
            Self::GraceExpired => "grace_expired",
            Self::Suspended => "suspended",
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
    /// Bytes charged to them: their ledger attributions, plus every distinct pulled metadata
    /// blob they hold and are not themselves attributed.
    pub used: u64,
    /// When they first crossed the hard limit and have not been under it since, if ever.
    ///
    /// Kept by the store rather than derived, because "how long have you been over" cannot be
    /// computed from a current total.
    pub over_since: Option<Timestamp>,
    /// When an operator suspended the account, if it is suspended now.
    ///
    /// Independent of the usage: no charge or release sets or clears it, only
    /// [`QuotaStore::suspend`] and [`QuotaStore::unsuspend`].
    pub suspended_since: Option<Timestamp>,
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

/// One reservation past its deadline, as [`QuotaStore::expired_reservations`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredReservation {
    /// The account the reservation is charged to.
    pub user: UserId,
    /// The content address it reserves.
    pub address: ContentAddress,
    /// The bytes it reserves.
    pub size: u64,
    /// The deadline it passed.
    pub reserved_until: Timestamp,
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

    /// [`Self::charge`], recording the attribution as a reservation that lapses at `until`.
    ///
    /// The charge an upload session's creation makes: the bytes are not stored yet, so the row
    /// says so until [`Self::settle`] clears it. An address that is already attributed is
    /// answered [`ChargeOutcome::AlreadyAttributed`] and left exactly as it is — reserved or
    /// settled, and with its own deadline.
    fn reserve<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        until: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome>;

    /// Settle `address`'s reservation, whoever it is charged to: its bytes are committed.
    ///
    /// `true` when the address is attributed after the call — a reservation settled now, or an
    /// attribution that was settled already. `false` when nothing attributes the address, which
    /// a finalizer answers by charging the bytes it just committed: the reservation sweep may
    /// have released it between that finalizer's blob check and this call.
    fn settle<'a>(&'a self, address: &'a ContentAddress) -> StoreFuture<'a, bool>;

    /// Up to `limit` reservations whose deadline is at or before `now`, earliest deadline first,
    /// ties broken by address.
    ///
    /// Bounded by the deadline, not by the ledger: a settled attribution is never listed.
    fn expired_reservations(
        &self,
        now: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<ExpiredReservation>>;

    /// Move `address`'s reservation deadline to `until`, if it is still a reservation.
    ///
    /// `true` when a reservation was extended. A live session still declares the address, so
    /// the reservation is still owed. Never shortens a deadline.
    fn extend_reservation<'a>(
        &'a self,
        address: &'a ContentAddress,
        until: Timestamp,
    ) -> StoreFuture<'a, bool>;

    /// Release `user`'s reservation of `address`, only if it is still a reservation whose
    /// deadline is at or before `now`.
    ///
    /// `true` when something was released. The condition and the delete are one operation, so
    /// a [`Self::settle`] or [`Self::extend_reservation`] that lands first keeps the attribution
    /// and a second call releases nothing.
    fn release_expired<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        now: Timestamp,
    ) -> StoreFuture<'a, bool>;

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
    /// The collector's release, and it is a **separate operation** rather than a
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

    /// Suspend `user`'s quota as of `at`.
    ///
    /// `true` when this call suspended the account; `false` when it was already suspended, in
    /// which case the earlier instant is kept, so the record says when the suspension began
    /// rather than when it was last repeated. An account the ledger has never charged may be
    /// suspended too: suspension does not wait for the first upload.
    fn suspend<'a>(&'a self, user: &'a UserId, at: Timestamp) -> StoreFuture<'a, bool>;

    /// Lift a suspension of `user`'s quota.
    ///
    /// `true` when this call lifted one; `false` when the account was not suspended.
    fn unsuspend<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, bool>;
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

/// The state an account is in, given everything the ledger holds for it.
///
/// A suspension outranks the thresholds: a suspended account is [`QuotaState::Suspended`]
/// whatever it uses. Otherwise this is [`state_of`] over the usage and the crossing.
pub fn classify(usage: StoredUsage, now: Timestamp, limits: QuotaLimits) -> QuotaState {
    if usage.suspended_since.is_some() {
        return QuotaState::Suspended;
    }
    state_of(usage.used, usage.over_since, now, limits)
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
        WriteClass::MetadataGrowth => {
            !matches!(state, QuotaState::GraceExpired | QuotaState::Suspended)
        }
        // A suspension refuses an upload whatever it would cost.
        WriteClass::Upload if state == QuotaState::Suspended => false,
        // Checked against the projected total, not the current one: the point of enforcing at
        // session creation is that the declared size becomes the cap for the whole transfer.
        WriteClass::Upload => used.saturating_add(additional) < limits.hard_limit,
    }
}

/// The pulled metadata blobs an account holds, which the in-memory ledger counts in its usage.
///
/// The durable ledger reads the same fact from the federation tables in its usage statement; the
/// in-memory one is handed it, because its pulled albums are a separate double.
pub trait PulledHoldings: std::fmt::Debug + Send + Sync {
    /// Every distinct pulled metadata blob `user` holds through an album it subscribes to, live
    /// or lapsed, with its size.
    fn held_by<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, BTreeMap<ContentAddress, u64>>;
}

/// A deterministic [`PulledHoldings`]: which albums each account holds which blobs through.
#[derive(Debug, Default)]
pub struct InMemoryPulledHoldings {
    held: Mutex<BTreeMap<(UserId, String), BTreeMap<ContentAddress, u64>>>,
}

impl InMemoryPulledHoldings {
    /// No account holds anything.
    pub fn new() -> Self {
        Self::default()
    }

    /// `user` holds `address`, of `size` bytes, through `album`.
    pub fn hold(&self, user: &UserId, album: &str, address: &ContentAddress, size: u64) {
        lock(&self.held)
            .entry((user.clone(), album.to_owned()))
            .or_default()
            .insert(address.clone(), size);
    }

    /// `user` no longer subscribes to `album`.
    pub fn unsubscribe(&self, user: &UserId, album: &str) {
        lock(&self.held).remove(&(user.clone(), album.to_owned()));
    }
}

impl PulledHoldings for InMemoryPulledHoldings {
    fn held_by<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, BTreeMap<ContentAddress, u64>> {
        Box::pin(async move {
            let held = lock(&self.held);
            let mut blobs = BTreeMap::new();
            for ((holder, _), album) in held.iter() {
                if holder == user {
                    for (address, size) in album {
                        let entry = blobs.entry(address.clone()).or_insert(0);
                        *entry = (*entry).max(*size);
                    }
                }
            }
            Ok(blobs)
        })
    }
}

/// A deterministic in-memory adapter.
///
/// One mutex over both maps, which is what makes [`Self::charge`]'s check-and-debit atomic.
/// Pulled holdings ([`Self::with_pulled`]) are read before that lock is taken, so a pulled page
/// committed during a charge may be missed by it; the durable ledger reads them in the charge's
/// own statement.
#[derive(Debug, Default)]
pub struct InMemoryQuota {
    inner: Mutex<Inner>,
    pulled: Option<Arc<dyn PulledHoldings>>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Who each address's bytes are charged to, how many, and whether it is a reservation.
    attributed: BTreeMap<ContentAddress, Attribution>,
    /// Each user's total and the moment they went over.
    usage: BTreeMap<UserId, StoredUsage>,
}

/// One row of the in-memory ledger.
#[derive(Debug, Clone)]
struct Attribution {
    /// The account the bytes are charged to.
    user: UserId,
    /// How many.
    size: u64,
    /// The reservation's deadline, or `None` once the attribution is settled.
    reserved_until: Option<Timestamp>,
}

impl Inner {
    /// Attribute `row`'s bytes at `address` to its account, unless it is attributed already.
    ///
    /// The one body [`QuotaStore::charge`] and [`QuotaStore::reserve`] share; they differ only
    /// in the deadline the row carries. `pulled` is what the account holds through its
    /// subscriptions ([`PulledHoldings`]).
    fn attribute(
        &mut self,
        address: &ContentAddress,
        row: Attribution,
        at: Timestamp,
        limits: QuotaLimits,
        pulled: &BTreeMap<ContentAddress, u64>,
    ) -> ChargeOutcome {
        if self.attributed.contains_key(address) {
            return ChargeOutcome::AlreadyAttributed;
        }
        let (user, size) = (row.user.clone(), row.size);
        let user = &user;
        self.attributed.insert(address.clone(), row);
        // Totalled after the insert, so a pulled blob this charge now attributes to the account
        // is counted once, here, rather than once here and once as pulled.
        let pulled = self.pulled_total(user, pulled);
        let entry = self.usage.entry(user.clone()).or_default();
        entry.used = entry.used.saturating_add(size);
        let used = entry.used.saturating_add(pulled);
        // The moment they crossed, recorded on the crossing rather than on every write, so
        // the grace window measures from when it started.
        if used >= limits.hard_limit && entry.over_since.is_none() {
            entry.over_since = Some(at);
            tracing::info!(%user, used, pulled, "an account crossed its hard quota limit");
        }
        ChargeOutcome::Charged { used }
    }

    /// What `pulled`, the blobs `user` holds through its subscriptions, adds to its usage: every
    /// one the ledger does not already attribute to `user`.
    fn pulled_total(&self, user: &UserId, pulled: &BTreeMap<ContentAddress, u64>) -> u64 {
        pulled
            .iter()
            .filter(|(address, _)| {
                self.attributed
                    .get(*address)
                    .is_none_or(|row| &row.user != user)
            })
            .map(|(_, size)| *size)
            .fold(0_u64, u64::saturating_add)
    }

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

    /// This ledger, counting the pulled blobs `pulled` says each account holds.
    #[must_use]
    pub fn with_pulled(mut self, pulled: Arc<dyn PulledHoldings>) -> Self {
        self.pulled = Some(pulled);
        self
    }

    /// The pulled blobs `user` holds, or none when the ledger was assembled without them.
    async fn pulled_of(
        &self,
        user: &UserId,
    ) -> Result<BTreeMap<ContentAddress, u64>, crate::store::StoreError> {
        match &self.pulled {
            Some(pulled) => pulled.held_by(user).await,
            None => Ok(BTreeMap::new()),
        }
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
            let pulled = self.pulled_of(user).await?;
            let inner = lock(&self.inner);
            let mut usage = inner.usage.get(user).copied().unwrap_or_default();
            usage.used = usage.used.saturating_add(inner.pulled_total(user, &pulled));
            Ok(usage)
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
            let pulled = self.pulled_of(user).await?;
            let row = Attribution {
                user: user.clone(),
                size,
                reserved_until: None,
            };
            Ok(lock(&self.inner).attribute(address, row, at, limits, &pulled))
        })
    }

    fn reserve<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        until: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome> {
        Box::pin(async move {
            let pulled = self.pulled_of(user).await?;
            let row = Attribution {
                user: user.clone(),
                size,
                reserved_until: Some(until),
            };
            Ok(lock(&self.inner).attribute(address, row, at, limits, &pulled))
        })
    }

    fn settle<'a>(&'a self, address: &'a ContentAddress) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(row) = inner.attributed.get_mut(address) else {
                return Ok(false);
            };
            row.reserved_until = None;
            Ok(true)
        })
    }

    fn expired_reservations(
        &self,
        now: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<ExpiredReservation>> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            let mut expired: Vec<ExpiredReservation> = inner
                .attributed
                .iter()
                .filter_map(|(address, row)| {
                    let until = row.reserved_until.filter(|until| *until <= now)?;
                    Some(ExpiredReservation {
                        user: row.user.clone(),
                        address: address.clone(),
                        size: row.size,
                        reserved_until: until,
                    })
                })
                .collect();
            // The map is in address order, so a stable sort by deadline breaks ties by address.
            expired.sort_by_key(|reservation| reservation.reserved_until);
            expired.truncate(limit);
            Ok(expired)
        })
    }

    fn extend_reservation<'a>(
        &'a self,
        address: &'a ContentAddress,
        until: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(deadline) = inner
                .attributed
                .get_mut(address)
                .and_then(|row| row.reserved_until.as_mut())
            else {
                return Ok(false);
            };
            *deadline = (*deadline).max(until);
            Ok(true)
        })
    }

    fn release_expired<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        now: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let expired = inner.attributed.get(address).is_some_and(|row| {
                &row.user == user && row.reserved_until.is_some_and(|until| until <= now)
            });
            if !expired {
                return Ok(false);
            }
            if let Some(row) = inner.attributed.remove(address) {
                inner.credit(user, row.size);
            }
            Ok(true)
        })
    }

    fn release<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(row) = inner.attributed.get(address).cloned() else {
                return Ok(false);
            };
            if &row.user != user {
                return Ok(false);
            }
            inner.attributed.remove(address);
            inner.credit(user, row.size);
            Ok(true)
        })
    }

    fn release_attribution<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, Option<(UserId, u64)>> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(Attribution {
                user: owner, size, ..
            }) = inner.attributed.remove(address)
            else {
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

    fn suspend<'a>(&'a self, user: &'a UserId, at: Timestamp) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let entry = inner.usage.entry(user.clone()).or_default();
            if entry.suspended_since.is_some() {
                return Ok(false);
            }
            entry.suspended_since = Some(at);
            Ok(true)
        })
    }

    fn unsuspend<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            Ok(inner
                .usage
                .get_mut(user)
                .and_then(|entry| entry.suspended_since.take())
                .is_some())
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
/// # Suspension is checked first
///
/// A suspended account is refused before anything is charged, so a suspension refuses even an
/// upload that would have merged. The read is not atomic with the charge: an operator's
/// suspension takes effect for the sessions opened after it is recorded, and a session already
/// opening as it lands may still open, exactly as a session already open may finish.
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
    admit_upload(context, user, address, size, false).await
}

/// [`charge_upload`] for an upload session, whose bytes are not stored yet: the charge is a
/// reservation ([`QuotaStore::reserve`]) that lapses at [`reservation_deadline`] unless a
/// finalization settles it.
///
/// # Errors
///
/// Propagates the ledger's failure. Nothing is charged when it does.
pub async fn reserve_upload(
    context: &QuotaContext,
    user: &UserId,
    address: &ContentAddress,
    size: u64,
) -> Result<UploadCharge, crate::store::StoreError> {
    admit_upload(context, user, address, size, true).await
}

/// When a reservation made at `now` lapses: one session lifetime cap later.
///
/// A session declaring the address may live that long, so a reservation is never swept before
/// the session it was made for has expired. A live session admitted later against the same
/// reservation is the sweep's to see; it extends the deadline rather than releasing.
pub fn reservation_deadline(now: Timestamp) -> Timestamp {
    now.checked_add(crate::store::upload::LIFETIME_CAP)
        .unwrap_or(Timestamp::MAX)
}

/// The body [`charge_upload`] and [`reserve_upload`] share.
async fn admit_upload(
    context: &QuotaContext,
    user: &UserId,
    address: &ContentAddress,
    size: u64,
    reserve: bool,
) -> Result<UploadCharge, crate::store::StoreError> {
    let limits = context.limits();
    if context
        .quotas()
        .usage(user)
        .await?
        .suspended_since
        .is_some()
    {
        tracing::info!(%user, size, "an upload was refused: the account's quota is suspended");
        return Ok(UploadCharge::Suspended);
    }
    let now = context.clock().now();
    let outcome = if reserve {
        context
            .quotas()
            .reserve(user, address, size, now, reservation_deadline(now), limits)
            .await?
    } else {
        context
            .quotas()
            .charge(user, address, size, now, limits)
            .await?
    };
    let ChargeOutcome::Charged { used } = outcome else {
        // Somebody already holds these bytes. A merge is not storage, so there is nothing to
        // refuse and nothing to release.
        return Ok(UploadCharge::AlreadyAttributed);
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
    /// The session may open: the bytes were charged to this user by this call.
    ///
    /// A caller whose write then fails before the bytes land may [`QuotaStore::release`] them,
    /// because the debit is its own.
    Admitted,
    /// The session may open: the bytes were already attributed — to this user or to another —
    /// so nothing was debited.
    ///
    /// Distinct from [`Self::Admitted`] so a caller undoing a failed write never releases an
    /// attribution an earlier write made. One value for both owners, as
    /// [`ChargeOutcome::AlreadyAttributed`] is.
    AlreadyAttributed,
    /// It would cross the hard limit. Nothing is charged — the debit was released.
    Refused,
    /// The account's quota is suspended. Nothing was charged.
    Suspended,
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
    Ok(classify(usage, context.clock().now(), context.limits()))
}

pub mod conformance;
pub mod postgres;

pub use self::postgres::PostgresQuota;

#[cfg(test)]
mod tests;
