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
//! # The map of windows is bounded, twice
//!
//! A counter's *key* is frequently derived from something a caller sent — a share-link id, a
//! challenge id, a redirect host. A key space every caller can extend is a map that only grows,
//! and this one is process-wide and shared by every limiter on the surface, so growth here is
//! not one feature's problem. Two bounds, the same pair
//! [`InMemoryOidcAuthorizations`](crate::store::memory::InMemoryOidcAuthorizations) carries:
//!
//! - **Purged on every write.** A window whose budget has lapsed decides nothing — [`verdict`]
//!   already treats it as absent — so it is dropped rather than kept as a row nobody reads. The
//!   map holds live windows plus whatever lapsed since the last hit, never everything ever
//!   counted.
//! - **A ceiling.** Past [`WINDOW_CEILING`] a key that has no window yet is refused with
//!   [`StoreError::Rejected`], while every key that already has one keeps counting. Callers
//!   treat a counter error as a refusal, so a full map fails *closed*: a limiter under
//!   memory pressure denies rather than waves through, and an attacker cannot switch a limiter
//!   off by loading the store.
//!
//! Every derived key should still be bounded at its call site, as the OIDC authorize's is by
//! validating the redirect before it charges. These two are the facility's defence in depth,
//! not a licence to key a counter on an arbitrary string.

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
    EnrollmentRedemption(String),
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

/// How many distinct keys the in-memory counters will hold windows for at once.
///
/// A hundred thousand. A window is a key plus twelve bytes, so this is single-digit megabytes at
/// the bound; and because windows are purged as they lapse, reaching it means a hundred thousand
/// *simultaneously live* windows, which at the shortest budget on the surface (one minute) is a
/// rate no self-hosted deployment produces honestly. It is a backstop for a key derived from
/// something a caller sends, not the primary defence — that is bounding the key where it is
/// built.
pub const WINDOW_CEILING: usize = 100_000;

/// A deterministic in-memory adapter.
///
/// Windows are purged as they lapse and the map is bounded; see the module docs.
#[derive(Debug)]
pub struct InMemoryCounters {
    ceiling: usize,
    windows: Mutex<BTreeMap<CounterKey, Window>>,
}

impl Default for InMemoryCounters {
    fn default() -> Self {
        Self {
            ceiling: WINDOW_CEILING,
            windows: Mutex::new(BTreeMap::new()),
        }
    }
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

impl InMemoryCounters {
    /// An empty set of counters, bounded at [`WINDOW_CEILING`].
    pub fn new() -> Self {
        Self::default()
    }

    /// The same counters holding windows for at most `ceiling` keys.
    #[must_use]
    pub fn with_ceiling(mut self, ceiling: usize) -> Self {
        self.ceiling = ceiling;
        self
    }

    /// How many distinct keys hold a window right now.
    ///
    /// For tests that assert the *cardinality* of the key space rather than any one verdict —
    /// the property a caller-controlled key silently destroys.
    pub fn len(&self) -> usize {
        lock(&self.windows).len()
    }

    /// Whether no key holds a window.
    pub fn is_empty(&self) -> bool {
        lock(&self.windows).is_empty()
    }

    /// Drop every window whose budget has lapsed.
    fn purge(windows: &mut BTreeMap<CounterKey, Window>, now: Timestamp) {
        windows.retain(|_, window| now < window.purge_after);
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
            if windows.len() >= self.ceiling && !windows.contains_key(key) {
                tracing::warn!(
                    counter = key.as_str(),
                    windows = windows.len(),
                    ceiling = self.ceiling,
                    "the counter store is full; a hit was refused rather than counted"
                );
                return Err(crate::store::StoreError::Rejected {
                    store: "counters",
                    detail: format!("{} open windows is the ceiling", self.ceiling),
                });
            }
            let (decision, live) = verdict(windows.get(key).copied(), budget, at);

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
                    windows.insert(key.clone(), updated);
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
            Ok(verdict(windows.get(key).copied(), budget, at).0)
        })
    }

    fn reset<'a>(&'a self, key: &'a CounterKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            if lock(&self.windows).remove(key).is_some() {
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
}

pub mod budgets;

#[cfg(test)]
mod tests;
