//! Rate-limit and attempt counters (`S-C32`) — the fourth thing the Salvo grab-bag carried.
//!
//! # Why this is a port of its own and not a field on another
//!
//! `S-C29` gave homes to three of `SessionStorage`'s four responsibilities — session records,
//! the per-user index, and the ceremony records — and deliberately left this one out. Counters
//! are not records, and the difference is not stylistic: **a lost record is a ceremony the user
//! retries; a lost increment is one more password guess than the policy allows.** Folding them
//! into [`AuthStateStore`](crate::store::AuthStateStore) would have rebuilt the grab-bag one
//! field at a time, which is the thing `S-C29` exists to delete.
//!
//! # Increment and decide are one operation, always
//!
//! [`CounterStore::hit`] returns the verdict, not the count. A caller that read a counter,
//! compared it to a limit, and then incremented would let every request in a burst read the
//! same under-limit value — which is precisely the burst a limiter exists to stop, and the
//! reason "rate limiting" implemented as read-then-write is a limiter in name only.
//!
//! Nothing here exposes a bare read against which a caller could make that mistake.
//! [`CounterStore::peek`] exists and returns a [`Verdict`] rather than a number, so the worst a
//! caller can do with it is decide the same way twice.
//!
//! # The window belongs to the limit, never to the caller
//!
//! A [`Budget`] carries its own window and its own ceiling, and a [`CounterKey`] names what is
//! being limited. There is no `hit(key, limit, window)` overload, for the same reason the
//! ceremony stores take no TTL argument: a window a caller supplies is a window two call sites
//! eventually disagree about, and the disagreement is invisible until somebody is limited at the
//! wrong rate.
//!
//! # Fixed windows, stated plainly
//!
//! This is a **fixed-window** counter: the window starts at the first hit and resets when it
//! passes. That admits up to twice the budget across a window boundary — the classic fixed-window
//! burst — and it is chosen anyway because the alternatives (sliding logs, token buckets) either
//! store per-request state or need a background refill, and both are a larger promise than a
//! v1 abuse gate needs. Where the doubled burst would matter the budget is halved rather than
//! the algorithm changed, and this paragraph is the record of that trade rather than a comment
//! somebody later mistakes for a bug.
//!
//! # The map of windows is bounded, twice — and partitioned, so the bounds are not shared
//!
//! A counter's *key* is frequently derived from something a caller sent — a share-link id, an
//! enrollment code, a redirect host. A key space every caller can extend is a map that only
//! grows, and this one is process-wide and shared by every limiter on the surface, so growth
//! here is not one feature's problem. Two bounds, the same pair
//! [`InMemoryOidcAuthorizations`](crate::store::memory::InMemoryOidcAuthorizations) carries:
//!
//! - **Purged on every write.** A window whose budget has lapsed decides nothing — [`verdict`]
//!   already treats it as absent — so it is dropped rather than kept as a row nobody reads. The
//!   map holds live windows plus whatever lapsed since the last hit, never everything ever
//!   counted.
//! - **A ceiling.** Past its ceiling a key that has no window yet is refused with
//!   [`StoreError::Rejected`], while every key that already has one keeps counting. Callers
//!   treat a counter error as a refusal, so a full partition fails *closed*: a limiter under
//!   memory pressure denies rather than waves through, and an attacker cannot switch a limiter
//!   off by loading the store.
//!
//! # The ceiling is per [`CounterKey`] variant, never one number for everything
//!
//! One shared ceiling makes every limiter share a fate. Three surfaces charge a caller-controlled
//! key *before* resolving what it names — the share path, the drop path and the enrollment
//! redemption — and the drop path's window is an hour long, so it is the cheapest of them to
//! hold saturated. Under a single ceiling, a flood against that one surface would refuse a
//! **first** key to every other: a first-time share view, a first enrollment redemption after a
//! reboot, the first OIDC sign-in of the day. Each of those maps a counter error to a fail-closed
//! `500`/`503`, so the weakest surface would decide the availability of all four.
//!
//! So the store holds one partition per variant, each with [`CounterKey::ceiling`] sized from
//! that variant's own window and its own plausible rate of *distinct* keys — see the constants
//! below for the arithmetic. Filling one partition refuses new keys in that partition only. The
//! totals are deliberately close to the single ceiling they replace, because the point is not to
//! hold more windows; it is that the windows one surface holds are not the windows another
//! surface is denied.
//!
//! # What a legitimate caller experiences when a ceiling bites
//!
//! The paragraphs above describe the mechanism. This is the consequence, which is the part worth
//! knowing at three in the morning.
//!
//! Partitioning bounds the blast radius; it does not make the flooded surface well. `DropLink`
//! is the cheapest partition to hold saturated — twenty thousand fabricated but well-formed ids
//! across an hour-long window, under six a second — and while it is saturated, every visitor
//! arriving at a drop link the store holds no window for is refused. That is a **first-time**
//! visitor: a link already being counted keeps being counted, so the flood cannot evict anyone
//! it has not already locked out.
//!
//! Those callers are told `429` with an `error.*_at_capacity` code and a `retry_after`, **not**
//! the `500 error.*_unavailable` a broken store renders. The refusal is fail-closed either way;
//! what changes is that a client can back off instead of reporting an outage, and an operator
//! paged on `5xx` can tell "saturated by design" from "the store is down" without reading a
//! server-side `WARN` and inferring it. The distinction is the whole reason the ceiling refusal
//! has a code of its own.
//!
//! What partitioning is *not* is a fix for the flood. A per-source key is what would bound it,
//! and all three source keys wait on a trusted client address this server does not have.
//!
//! # One lock, and what that does and does not cover
//!
//! Every partition lives behind the same [`Mutex`]. Admission is genuinely independent — one
//! partition's occupancy is invisible to another's ceiling — but *latency* is not: a sustained
//! flood against one key serialises `hit`, `peek` and `reset` for every other. The critical
//! section holds no `.await` and does `O(log n)` work over at most twenty thousand entries, so at
//! these sizes it is contention rather than denial. Stated because the claim above ("the windows
//! one surface holds are not the windows another is denied") is about admission and should not be
//! read as a latency guarantee. Per-partition locking is deferred to issue #477, not overlooked.
//!
//! This is defence in depth, not a licence. Every derived key should still be bounded where it
//! is built — the OIDC authorize validates the redirect before it charges
//! ([`CounterKey::OidcAuthorizeRefused`]), and the enrollment redemption shape-checks the code
//! before it charges ([`CounterKey::EnrollmentRedemptionMalformed`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};

use crate::store::{StoreFuture, UserId};

/// What is being limited, and for whom.
///
/// A closed enum rather than a string key. The Salvo grab-bag namespaced its counters by
/// hand-formatted strings, so two call sites that formatted a key differently silently kept two
/// counters — and the one that mattered was whichever the *attacker* did not hit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CounterKey {
    /// Failed sign-in attempts for one account.
    LoginAttempts(UserId),
    /// Enrollment-code redemptions against one pending enrollment (`S-C7`, invariant 31's
    /// sibling in the enrollment contract).
    ///
    /// Built only from a code that passed the route's shape check, for the reason
    /// [`Self::OidcAuthorize`] is built only from an admitted redirect host: the presented code
    /// is caller-supplied, and a counter keyed on an unchecked one is a partition an
    /// unauthenticated caller fills a row at a time. Anything malformed goes to
    /// [`Self::EnrollmentRedemptionMalformed`].
    EnrollmentRedemption(String),
    /// Every enrollment redemption presenting a code that is not even shaped like one (`S-C7`).
    ///
    /// One bucket, as [`Self::OidcAuthorizeRefused`] is one bucket, and for the same reasons:
    /// a malformed attempt must still be throttled, and it must not be throttled *per code*,
    /// because the code is whatever the caller typed.
    EnrollmentRedemptionMalformed,
    /// Requests against one share link's opaque id (`S-C4`).
    ShareLink(String),
    /// Requests from one source address, on the public share path.
    ShareSource(String),
    /// Drop-session creations against one upload link (`S-C5`, invariant 31).
    DropLink(String),
    /// Drop-session creations from one source address (`S-C5`, invariant 31).
    DropSource(String),
    /// Deep storage verifications for one account (`S-C41`).
    DeepVerify(UserId),
    /// Code attempts against one second-factor challenge (`S-C55`).
    ///
    /// Keyed on the **challenge** and not the account, deliberately. A per-account key would let
    /// anyone who knows an address exhaust its budget with first-factor sign-ins they cannot
    /// complete, locking the owner out of an account whose password the attacker does not have.
    /// The challenge id is minted per ceremony and lives five minutes, so the budget it carries
    /// bounds exactly the thing being guessed: six digits, against one half-finished sign-in.
    SecondFactor(String),
    /// Account registrations from one source address (`S-C53`).
    ///
    /// Declared and consumed nowhere, like its two siblings above, and for the same reason: the
    /// key names a fact this server does not have behind an unconfigured proxy chain. Registration
    /// is the one **unauthenticated write** on the surface, so it is the place that fact is most
    /// missed — recorded here rather than replaced by an email-keyed limiter, which would bound
    /// repeated probes against one address while doing nothing about a sweep across many.
    RegistrationSource(String),
    /// Begun OIDC ceremonies naming one **admitted** redirect host (`S-N1`).
    ///
    /// Keyed on the redirect URI's host, and constructed only after
    /// [`IdentityProvider::admits_redirect`](crate::auth::oidc::IdentityProvider::admits_redirect)
    /// has said so. That ordering is load-bearing rather than tidy: the policy admits the
    /// configured redirect and the two loopback literals, so **downstream of validation** the key
    /// space is three buckets and the budget is, in effect, a deployment-wide ceiling on how fast
    /// pending ceremonies can be begun — which is what bounds the ceremony store's growth.
    /// Upstream of validation the host is an arbitrary caller-supplied string, and a counter
    /// keyed on one is a map an unauthenticated caller grows a row at a time. Every refusal goes
    /// to [`Self::OidcAuthorizeRefused`] instead.
    ///
    /// A per-source key is the better one and is waiting on the same missing fact as
    /// [`Self::RegistrationSource`].
    OidcAuthorize(String),
    /// Every OIDC authorize whose redirect the policy refused, in one bucket (`S-N1`).
    ///
    /// A refusal must still be throttled — otherwise the cheapest request on the surface is the
    /// one nothing counts — but it must not be throttled *per host*, because the host of a
    /// refused redirect is whatever the caller typed. So refusals share one deployment-wide
    /// window. That is deliberately blunt: it means a flood of invalid redirects can spend the
    /// refusal budget for everybody. It costs nothing real, because a client whose redirect the
    /// deployment admits never charges this bucket at all — only misconfigured and abusive
    /// callers do, and a misconfigured client's remedy is to be configured.
    ///
    /// A unit variant rather than `OidcAuthorize("<refused>")`: this enum exists because the
    /// retired surface namespaced counters with hand-formatted strings, and a sentinel string is
    /// that mistake with a nicer name.
    OidcAuthorizeRefused,
}

impl CounterKey {
    /// The name this key travels under, for a log field.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LoginAttempts(_) => "login_attempts",
            Self::EnrollmentRedemption(_) => "enrollment_redemption",
            Self::EnrollmentRedemptionMalformed => "enrollment_redemption_malformed",
            Self::ShareLink(_) => "share_link",
            Self::ShareSource(_) => "share_source",
            Self::DropLink(_) => "drop_link",
            Self::DropSource(_) => "drop_source",
            Self::DeepVerify(_) => "deep_verify",
            Self::SecondFactor(_) => "second_factor",
            Self::RegistrationSource(_) => "registration_source",
            Self::OidcAuthorize(_) => "oidc_authorize",
            Self::OidcAuthorizeRefused => "oidc_authorize_refused",
        }
    }

    /// How many simultaneously live windows this variant's partition may hold.
    ///
    /// Per variant and never one number for all of them: see the module docs for why a shared
    /// ceiling is a shared fate, and [`ceilings`] for each number's arithmetic.
    pub fn ceiling(&self) -> usize {
        match self {
            Self::LoginAttempts(_) => ceilings::LOGIN_ATTEMPTS,
            Self::EnrollmentRedemption(_) => ceilings::ENROLLMENT_REDEMPTION,
            Self::EnrollmentRedemptionMalformed => ceilings::ENROLLMENT_REDEMPTION_MALFORMED,
            Self::ShareLink(_) => ceilings::SHARE_LINK,
            Self::DropLink(_) => ceilings::DROP_LINK,
            Self::ShareSource(_) | Self::DropSource(_) | Self::RegistrationSource(_) => {
                ceilings::SOURCE_ADDRESS
            }
            Self::DeepVerify(_) => ceilings::DEEP_VERIFY,
            Self::SecondFactor(_) => ceilings::SECOND_FACTOR,
            Self::OidcAuthorize(_) => ceilings::OIDC_AUTHORIZE,
            Self::OidcAuthorizeRefused => ceilings::OIDC_AUTHORIZE_REFUSED,
        }
    }
}

/// How many, and over how long.
///
/// Carried by the *limit*, never passed per call. Two call sites that could each supply a window
/// are two call sites that will eventually disagree, and the disagreement is invisible until
/// somebody is throttled at the wrong rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// How many hits the window admits.
    pub limit: u32,
    /// How long the window lasts, measured from its first hit.
    pub window: SignedDuration,
}

impl Budget {
    /// A budget of `limit` hits per `window`.
    pub const fn new(limit: u32, window: SignedDuration) -> Self {
        Self { limit, window }
    }
}

/// Whether the caller may proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Under budget. `remaining` is how many hits are left in this window.
    Admitted {
        /// Hits left before the budget is spent.
        remaining: u32,
    },
    /// Over budget until `retry_after`.
    Limited {
        /// When the window resets and the caller may try again.
        retry_after: Timestamp,
    },
}

impl Verdict {
    /// Whether the caller may proceed.
    pub fn admits(self) -> bool {
        matches!(self, Self::Admitted { .. })
    }
}

/// The counter port.
pub trait CounterStore: std::fmt::Debug + Send + Sync {
    /// Charge one hit against `key`'s budget and decide, as one operation.
    ///
    /// **The two together, never separately.** Read-then-increment lets every request in a burst
    /// read the same under-limit value, which is the burst the limiter exists to stop. Every
    /// adapter owes this atomically; the in-memory one gets it from a mutex and Valkey from
    /// `INCR` plus a first-hit `EXPIRE`.
    ///
    /// An adapter may answer [`StoreError::Rejected`](crate::store::StoreError::Rejected) when
    /// it cannot hold another key's window. That is an error and not a [`Verdict`], deliberately:
    /// the caller's rule for *any* counter failure is already "refuse", so a full store denies
    /// through the path that is documented to fail closed rather than through a new one somebody
    /// could handle as an admission.
    fn hit<'a>(
        &'a self,
        key: &'a CounterKey,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict>;

    /// The verdict a hit *would* get, without charging one.
    ///
    /// Returns a [`Verdict`] and not a count, deliberately: handing back a number is handing
    /// back the read half of a read-then-write, and somebody would eventually build a limiter
    /// out of it.
    fn peek<'a>(
        &'a self,
        key: &'a CounterKey,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict>;

    /// Clear `key`'s window.
    ///
    /// What a *successful* sign-in does to a failed-attempt counter: the policy counts
    /// consecutive failures, so a success is not merely one more event, it ends the run.
    fn reset<'a>(&'a self, key: &'a CounterKey) -> StoreFuture<'a, ()>;
}

/// How many simultaneously live windows each [`CounterKey`] variant may hold.
///
/// Each is that variant's window length multiplied by a stated rate of *distinct* keys, rounded
/// up for headroom. The budget bounds hits **per key**; it says nothing about how many keys
/// exist, so the key rate is the assumption each number is built on and each is written down.
pub mod ceilings {
    /// [`CounterKey::LoginAttempts`](super::CounterKey::LoginAttempts) — 15-minute window, keyed
    /// on an account.
    ///
    /// 900 s × ~1 account entering a failure window per second = 900. Rounded to ten thousand:
    /// the key is an account id, so the true bound is the size of the directory, and a
    /// deployment large enough to exceed this has other numbers to raise first.
    pub const LOGIN_ATTEMPTS: usize = 10_000;

    /// [`CounterKey::EnrollmentRedemption`](super::CounterKey::EnrollmentRedemption) —
    /// 10-minute window, keyed on a shape-checked code.
    ///
    /// 600 s × ~1 code presented per second = 600. Rounded to five thousand. Device enrollment
    /// is a rare, deliberate act: a deployment redeeming five thousand distinct codes inside ten
    /// minutes is not one this number is failing.
    pub const ENROLLMENT_REDEMPTION: usize = 5_000;

    /// [`CounterKey::EnrollmentRedemptionMalformed`](super::CounterKey::EnrollmentRedemptionMalformed)
    /// — one key exists, so one window.
    pub const ENROLLMENT_REDEMPTION_MALFORMED: usize = 1;

    /// [`CounterKey::ShareLink`](super::CounterKey::ShareLink) — 1-minute window, keyed on a
    /// caller-supplied opaque id.
    ///
    /// 60 s × ~100 distinct links opened per second = 6 000. Rounded to twenty thousand for
    /// three-fold headroom, because this is the surface a public link is *meant* to be hit on.
    pub const SHARE_LINK: usize = 20_000;

    /// [`CounterKey::DropLink`](super::CounterKey::DropLink) — 1-hour window, keyed on a
    /// caller-supplied opaque id.
    ///
    /// 3 600 s × ~1 distinct link receiving a session per second = 3 600. Rounded to twenty
    /// thousand. The hour-long window makes this the cheapest partition to hold saturated — at
    /// twenty thousand ids an hour, under six a second — which is exactly why it is a partition:
    /// saturating it costs the drop path its first-time keys and costs no other surface
    /// anything.
    pub const DROP_LINK: usize = 20_000;

    /// [`CounterKey::SecondFactor`](super::CounterKey::SecondFactor) — 5-minute window, keyed on
    /// a server-minted challenge id.
    ///
    /// 300 s × ~10 sign-ins reaching a second factor per second = 3 000. Rounded to ten
    /// thousand. Not caller-controlled: a challenge id comes off a token this server signed.
    pub const SECOND_FACTOR: usize = 10_000;

    /// [`CounterKey::DeepVerify`](super::CounterKey::DeepVerify) — 1-hour window, keyed on an
    /// authenticated account.
    ///
    /// Bounded by the directory, as `LOGIN_ATTEMPTS` is, and reached only by accounts that asked
    /// for a deep scan in the last hour.
    pub const DEEP_VERIFY: usize = 10_000;

    /// The three source-address keys — 1-minute and 1-hour windows, keyed on a client address.
    ///
    /// Charged nowhere yet: all three wait on a trusted client address this server does not have
    /// behind an unconfigured proxy chain. Sized for the day one arrives — distinct addresses in
    /// the window, which for a self-hosted deployment is thousands, not millions.
    pub const SOURCE_ADDRESS: usize = 10_000;

    /// [`CounterKey::OidcAuthorize`](super::CounterKey::OidcAuthorize) — 1-minute window, keyed
    /// on an **admitted** redirect host.
    ///
    /// Three keys can exist: the configured redirect's host and the two loopback literals. Set
    /// to sixteen rather than three so that changing `OIDC_REDIRECT_URL` while a window is open,
    /// or a provider spelling `[::1]` differently, meets headroom instead of a cliff — and small
    /// enough that it is visibly a *bounded* key rather than a hopeful one.
    pub const OIDC_AUTHORIZE: usize = 16;

    /// [`CounterKey::OidcAuthorizeRefused`](super::CounterKey::OidcAuthorizeRefused) — one key
    /// exists, so one window.
    pub const OIDC_AUTHORIZE_REFUSED: usize = 1;
}

/// A deterministic in-memory adapter.
///
/// Windows are purged as they lapse, and each [`CounterKey`] variant is bounded in a partition of
/// its own; see the module docs.
#[derive(Debug, Default)]
pub struct InMemoryCounters {
    /// Set by [`InMemoryCounters::with_ceiling`], and then the ceiling of **every** partition.
    /// `None` in production, where each variant carries its own.
    ceiling_override: Option<usize>,
    /// Partitioned by [`CounterKey::as_str`], so one variant's occupancy is invisible to
    /// another's ceiling. An emptied partition is dropped by the purge rather than left behind.
    windows: Mutex<BTreeMap<&'static str, BTreeMap<CounterKey, Window>>>,
}

/// One key's open window.
#[derive(Debug, Clone, Copy)]
struct Window {
    hits: u32,
    opened_at: Timestamp,
    /// When this window may be dropped, from the budget in force when it opened.
    ///
    /// A **purge hint only.** Admission is always recomputed by [`verdict`] from `opened_at`
    /// against the budget the caller supplies, so re-tuning a budget takes effect on the next
    /// hit exactly as it did before this field existed; all this decides is when a row nobody
    /// will read again is collected.
    purge_after: Timestamp,
}

/// The partitioned window map: one inner map per [`CounterKey`] variant.
type Partitions = BTreeMap<&'static str, BTreeMap<CounterKey, Window>>;

impl InMemoryCounters {
    /// An empty set of counters, each variant bounded by its own [`CounterKey::ceiling`].
    pub fn new() -> Self {
        Self::default()
    }

    /// The same counters with **every** partition bounded at `ceiling` instead.
    ///
    /// A tuning and testing affordance: it makes the partition boundary observable without
    /// writing twenty thousand keys. Production leaves it unset, so each variant carries the
    /// number its own window and key rate justify.
    #[must_use]
    pub fn with_ceiling(mut self, ceiling: usize) -> Self {
        self.ceiling_override = Some(ceiling);
        self
    }

    /// How many distinct keys hold a window right now, across every partition.
    ///
    /// For tests that assert the *cardinality* of the key space rather than any one verdict —
    /// the property a caller-controlled key silently destroys.
    pub fn len(&self) -> usize {
        lock(&self.windows).values().map(BTreeMap::len).sum()
    }

    /// How many distinct keys hold a window in `key`'s partition.
    ///
    /// The number the ceiling is actually compared against, so a test can assert that filling
    /// one surface left another's occupancy alone.
    pub fn len_of(&self, key: &CounterKey) -> usize {
        lock(&self.windows)
            .get(key.as_str())
            .map_or(0, BTreeMap::len)
    }

    /// Whether no key holds a window.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// This key's partition ceiling, or the override every partition shares when one is set.
    fn ceiling_for(&self, key: &CounterKey) -> usize {
        self.ceiling_override.unwrap_or_else(|| key.ceiling())
    }

    /// Drop every window whose budget has lapsed, and every partition thereby emptied.
    fn purge(windows: &mut Partitions, now: Timestamp) {
        windows.retain(|_, partition| {
            partition.retain(|_, window| now < window.purge_after);
            !partition.is_empty()
        });
    }
}

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Decide `window` against `budget` at `at`, treating an elapsed window as absent.
fn verdict(window: Option<Window>, budget: Budget, at: Timestamp) -> (Verdict, Option<Window>) {
    let live = window.filter(|open| at < crate::store::deadline(open.opened_at, budget.window));

    match live {
        Some(open) if open.hits >= budget.limit => (
            Verdict::Limited {
                retry_after: crate::store::deadline(open.opened_at, budget.window),
            },
            Some(open),
        ),
        Some(open) => (
            Verdict::Admitted {
                remaining: budget.limit.saturating_sub(open.hits),
            },
            Some(open),
        ),
        // No window, or one that has passed. A fresh window starts at this hit.
        None => (
            Verdict::Admitted {
                remaining: budget.limit,
            },
            None,
        ),
    }
}

impl CounterStore for InMemoryCounters {
    fn hit<'a>(
        &'a self,
        key: &'a CounterKey,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        Box::pin(async move {
            let mut windows = lock(&self.windows);
            Self::purge(&mut windows, at);

            let held = windows
                .get(key.as_str())
                .and_then(|partition| partition.get(key))
                .copied();
            let ceiling = self.ceiling_for(key);
            let occupancy = windows.get(key.as_str()).map_or(0, BTreeMap::len);
            // Only a key with no window yet can be refused, and only by its own partition's
            // occupancy. A key already being counted keeps being counted, and another variant's
            // flood is not visible here at all.
            if held.is_none() && occupancy >= ceiling {
                tracing::warn!(
                    counter = key.as_str(),
                    windows = occupancy,
                    ceiling,
                    "a counter partition is full; a hit was refused rather than counted"
                );
                return Err(crate::store::StoreError::Rejected {
                    store: COUNTER_STORE,
                    detail: format!(
                        "{ceiling} open windows is the ceiling for `{}`",
                        key.as_str()
                    ),
                });
            }
            let (decision, live) = verdict(held, budget, at);

            match decision {
                Verdict::Limited { retry_after } => {
                    tracing::info!(
                        counter = key.as_str(),
                        %retry_after,
                        "a rate limit engaged"
                    );
                    Ok(Verdict::Limited { retry_after })
                }
                Verdict::Admitted { .. } => {
                    // The charge and the decision are one critical section. A caller cannot
                    // observe the state in between, which is the whole property.
                    let updated = match live {
                        Some(open) => Window {
                            hits: open.hits.saturating_add(1),
                            opened_at: open.opened_at,
                            purge_after: crate::store::deadline(open.opened_at, budget.window),
                        },
                        None => Window {
                            hits: 1,
                            opened_at: at,
                            purge_after: crate::store::deadline(at, budget.window),
                        },
                    };
                    windows
                        .entry(key.as_str())
                        .or_default()
                        .insert(key.clone(), updated);
                    Ok(Verdict::Admitted {
                        remaining: budget.limit.saturating_sub(updated.hits),
                    })
                }
            }
        })
    }

    fn peek<'a>(
        &'a self,
        key: &'a CounterKey,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        Box::pin(async move {
            let windows = lock(&self.windows);
            let held = windows
                .get(key.as_str())
                .and_then(|partition| partition.get(key))
                .copied();
            Ok(verdict(held, budget, at).0)
        })
    }

    fn reset<'a>(&'a self, key: &'a CounterKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let mut windows = lock(&self.windows);
            if let Some(partition) = windows.get_mut(key.as_str())
                && partition.remove(key).is_some()
            {
                if partition.is_empty() {
                    windows.remove(key.as_str());
                }
                tracing::debug!(counter = key.as_str(), "a counter window was cleared");
            }
            Ok(())
        })
    }
}

/// The counter module's collaborators.
#[derive(Debug, Clone)]
pub struct CounterContext {
    counters: Arc<dyn CounterStore>,
    clock: Arc<dyn crate::store::Clock>,
}

impl CounterContext {
    /// Assembles the module.
    pub fn new(counters: Arc<dyn CounterStore>, clock: Arc<dyn crate::store::Clock>) -> Self {
        Self { counters, clock }
    }

    /// Charge one hit and decide.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](crate::store::StoreError) if the counter could not be reached.
    /// A caller **must** treat that as a refusal rather than as an admission: a limiter that
    /// fails open is a limiter an attacker turns off by loading the counter store.
    pub async fn hit(
        &self,
        key: &CounterKey,
        budget: Budget,
    ) -> Result<Verdict, crate::store::StoreError> {
        self.counters.hit(key, budget, self.clock.now()).await
    }

    /// The verdict a hit would get.
    ///
    /// # Errors
    ///
    /// As [`Self::hit`].
    pub async fn peek(
        &self,
        key: &CounterKey,
        budget: Budget,
    ) -> Result<Verdict, crate::store::StoreError> {
        self.counters.peek(key, budget, self.clock.now()).await
    }

    /// Clear a key's window.
    ///
    /// # Errors
    ///
    /// As [`Self::hit`].
    pub async fn reset(&self, key: &CounterKey) -> Result<(), crate::store::StoreError> {
        self.counters.reset(key).await
    }

    /// What a caller should be told about a failed [`Self::hit`].
    ///
    /// `Some(retry_after)` when the partition was full — the limiter working as designed, which
    /// a route renders `429 error.*_at_capacity` — and `None` when the store could not answer at
    /// all, which stays a `500`. One method rather than a predicate plus a clock read at three
    /// call sites, because the half that is easy to forget is the deadline.
    ///
    /// The refusal is fail-closed either way; this decides only the answer.
    pub fn capacity_refusal(
        &self,
        error: &crate::store::StoreError,
        budget: Budget,
    ) -> Option<Timestamp> {
        is_at_capacity(error).then(|| capacity_retry_after(self.clock.now(), budget))
    }
}

/// Whether a [`CounterStore`] failure was the partition ceiling rather than a broken store.
///
/// The two failures arrive as one `Result::Err` and mean opposite things to a caller: a full
/// partition is the limiter working as designed and clears on its own within the window, while
/// anything else is a store that could not answer. A route that renders both as `500` tells a
/// client to report an outage and tells an operator to go looking for one, so every route that
/// charges a caller-influenced key asks this and answers `429 error.*_at_capacity` when it is
/// true.
///
/// The refusal itself is fail-closed either way. This decides only what the caller is told.
pub fn is_at_capacity(error: &crate::store::StoreError) -> bool {
    matches!(
        error,
        crate::store::StoreError::Rejected { store, .. } if *store == COUNTER_STORE
    )
}

/// The `store` name [`InMemoryCounters`] refuses under, and [`is_at_capacity`] matches on.
pub const COUNTER_STORE: &str = "counters";

/// When a caller refused by a full partition may expect room, as an **upper** bound.
///
/// One window from now. A full partition is full of *live* windows, and the earliest of them
/// lapses no later than one window after it opened, so a caller that waits this long finds room
/// unless the flood is still running — in which case it finds the same honest `429` again.
pub fn capacity_retry_after(now: Timestamp, budget: Budget) -> Timestamp {
    crate::store::deadline(now, budget.window)
}

/// `at` as Unix seconds for a `retry_after` extension member.
///
/// Saturating at zero, as every other deadline on this surface does: a clock before the epoch is
/// a misconfiguration, and "retry now" is the safe reading of one.
pub fn unix_seconds(at: Timestamp) -> u64 {
    u64::try_from(at.as_second()).unwrap_or(0)
}

pub mod budgets;

#[cfg(test)]
mod tests;
