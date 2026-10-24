//! Rate-limit and attempt counters — the fourth thing the Salvo grab-bag carried.
//!
//! # Why this is a port of its own and not a field on another
//!
//! The state ports give homes to three of the old `SessionStorage`'s four responsibilities —
//! session records, the per-user index, and the ceremony records — and deliberately leave this
//! one out. Counters
//! are not records, and the difference is not stylistic: **a lost record is a ceremony the user
//! retries; a lost increment is one more password guess than the policy allows.** Folding them
//! into [`AuthStateStore`](crate::store::AuthStateStore) would have rebuilt the grab-bag one
//! field at a time, which is the thing the split state ports exist to avoid.
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
//! A counter's *key* is frequently derived from something a caller sent — a share-link id, a
//! drop-link id, a redirect host. A key space every caller can extend is a map that only
//! grows, and this one is process-wide and shared by every limiter on the surface, so growth
//! here is not one feature's problem. Two bounds, the same pair
//! [`InMemoryOidcAuthorizations`](crate::store::memory::InMemoryOidcAuthorizations) carries:
//!
//! - **Purged on every write.** A window whose budget has lapsed decides nothing — `verdict`
//!   already treats it as absent — so it is dropped rather than kept as a row nobody reads. The
//!   map holds live windows plus whatever lapsed since the last hit, never everything ever
//!   counted.
//! - **A ceiling.** Past its ceiling a key that has no window yet is refused with
//!   [`StoreError::Rejected`](crate::store::StoreError::Rejected), while every key that already has one keeps counting. Callers
//!   treat a counter error as a refusal, so a full partition fails *closed*: a limiter under
//!   memory pressure denies rather than waves through, and an attacker cannot switch a limiter
//!   off by loading the store.
//!
//! # The ceiling is per [`CounterKey`] variant, never one number for everything
//!
//! One shared ceiling makes every limiter share a fate. Two surfaces charge a caller-controlled
//! key *before* resolving what it names — the share path and the drop path (the enrollment
//! redemption did too, until issue #528 keyed it on the code's spelling instead) — and the drop
//! path's window is an hour long, so it is the cheapest of them to hold saturated. Under a single ceiling, a flood against that one surface would refuse a
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
//! What partitioning is *not* is a fix for the flood. A per-source key is what bounds it: the
//! share and drop paths charge [`CounterKey::ShareSource`] and [`CounterKey::DropSource`] first
//! (the upload writes charge [`CounterKey::UploadSessionsSource`] and
//! [`CounterKey::UploadChunksSource`] beside their per-account keys),
//! keyed on the client address [`crate::client_addr`] resolves through the deployment's trusted
//! proxies (#704), so one address cannot fill a partition faster than its own budget allows.
//!
//! # One lock per partition, and what that does and does not cover
//!
//! Each partition lives behind a [`Mutex`] of its own, in a fixed array built once and read
//! without a lock. Admission is independent — one partition's occupancy is invisible to
//! another's ceiling — and so is *latency*: a sustained flood against one key serialises `hit`,
//! `peek` and `reset` for that variant only, and a caller charging another variant never waits
//! on it. The atomicity [`CounterStore::hit`] promises survives the split because a key never
//! spans two partitions, so its charge and its decision are still one critical section.
//!
//! What a hit costs another partition is at most a purge of it, and only when that partition is
//! free: the lapsed windows of a partition nobody writes to are collected by whoever writes next
//! anywhere, through a `try_lock` that skips a partition someone holds rather than waiting on
//! it. Within one partition the flood still contends with itself — the critical section holds no
//! `.await` and does `O(log n)` work over at most that partition's ceiling — which is contention
//! the flood's own surface pays, bounded by the ceiling it is filling.
//!
//! This is defence in depth, not a licence. Every derived key should still be bounded where it
//! is built — the OIDC authorize validates the redirect before it charges
//! ([`CounterKey::OidcAuthorizeRefused`]), and the enrollment redemption keys on the shape of
//! the code rather than its value ([`CounterKey::EnrollmentRedemption`],
//! [`CounterKey::EnrollmentRedemptionMalformed`]).

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
    /// Enrollment-code redemptions presenting one [`EnrollmentSpelling`], deployment-wide
    /// (issue #528).
    ///
    /// Keyed on the **spelling** of the presented code and never on its value. A key built from
    /// the value is not guess-invariant: every wrong guess is a different string, so every wrong
    /// guess got a fresh budget and the limiter bounded nothing an attacker would do — the
    /// fault #528 reports. Nor can a wrong guess be charged to the pending enrollment it was
    /// aimed at, because a miss names no pending enrollment. What *is* invariant across guesses
    /// is the space being searched, so that is the key: one window per spelling, which bounds the
    /// guesses that can land on any one pending enrollment inside its lifetime (see
    /// [`budgets::ENROLLMENT_REDEMPTION`] for the arithmetic).
    ///
    /// The spelling comes from the route's shape check, for the reason [`Self::OidcAuthorize`]
    /// is built only from an admitted redirect host; anything malformed goes to
    /// [`Self::EnrollmentRedemptionMalformed`]. The key space is two values by construction.
    EnrollmentRedemption(EnrollmentSpelling),
    /// Every enrollment redemption presenting a code that is not even shaped like one.
    ///
    /// One bucket, as [`Self::OidcAuthorizeRefused`] is one bucket, and for the same reasons:
    /// a malformed attempt must still be throttled, and it must not be throttled *per code*,
    /// because the code is whatever the caller typed.
    EnrollmentRedemptionMalformed,
    /// Requests against one share link's opaque id.
    ShareLink(String),
    /// Requests from one source address, on the public share path (#704).
    ///
    /// Keyed on [`ClientAddr::source_key`](crate::client_addr::ClientAddr::source_key): the
    /// client the router resolved through the trusted proxies, never a forwarding header taken on
    /// its word.
    ShareSource(String),
    /// Drop-session creations against one upload link (invariant 31).
    DropLink(String),
    /// Drop-session creations from one source address (invariant 31, #704).
    ///
    /// Keyed as [`Self::ShareSource`] is.
    DropSource(String),
    /// Deep storage verifications for one account.
    DeepVerify(UserId),
    /// Upload sessions opened by one account (transfer/upload.md, Hostile Clients).
    ///
    /// Keyed on the authenticated uploader, never on the session or the hash: both are values
    /// the caller mints per request, so a budget keyed on either is a budget each request
    /// renews.
    UploadSessions(UserId),
    /// Chunks appended by one account, across every session it holds (transfer/upload.md,
    /// Hostile Clients).
    ///
    /// Per account and not per session, for the reason [`Self::UploadSessions`] is: a session
    /// is cheap to open, so a per-session budget is one a flood multiplies by opening more.
    UploadChunks(UserId),
    /// Upload sessions opened from one source address, across every account behind it
    /// (transfer/upload.md, Hostile Clients; #718).
    ///
    /// Keyed as [`Self::ShareSource`] is. Charged **after** [`Self::UploadSessions`], so a
    /// request the account's own budget refuses never reaches this one: one account flooding
    /// from a shared address spends at most its own allowance of the address's, and the other
    /// accounts behind that address keep the rest.
    UploadSessionsSource(String),
    /// Chunks appended from one source address, across every account and session behind it
    /// (transfer/upload.md, Hostile Clients; #718).
    ///
    /// Keyed and ordered as [`Self::UploadSessionsSource`] is, after [`Self::UploadChunks`].
    UploadChunksSource(String),
    /// Code attempts against one second-factor challenge.
    ///
    /// Keyed on the **challenge** and not the account, deliberately. A per-account key would let
    /// anyone who knows an address exhaust its budget with first-factor sign-ins they cannot
    /// complete, locking the owner out of an account whose password the attacker does not have.
    /// The challenge id is minted per ceremony and lives five minutes, so the budget it carries
    /// bounds exactly the thing being guessed: six digits, against one half-finished sign-in.
    SecondFactor(String),
    /// Account registrations from one source address.
    ///
    /// Declared and consumed nowhere yet. The client address it needs now exists
    /// ([`crate::client_addr`], #704) and the share and drop paths charge their source keys with
    /// it; registration is the one **unauthenticated write** on the surface, so it is the next
    /// place that fact is owed — recorded here rather than replaced by an email-keyed limiter,
    /// which would bound repeated probes against one address while doing nothing about a sweep
    /// across many.
    RegistrationSource(String),
    /// Begun OIDC ceremonies naming one **admitted** redirect host.
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
    /// Every OIDC authorize whose redirect the policy refused, in one bucket.
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
    /// Every OIDC device authorization begun, deployment-wide (issue #635).
    ///
    /// One bucket, because the begin carries nothing a caller does not choose: there is no
    /// redirect host to key on, and the binding challenge is a fresh random hash per ceremony,
    /// so keying on it would hand every request a fresh allowance. It is the same unauthenticated
    /// write into a store the authorize is, so it is bounded the way the authorize's admitted
    /// path is in effect — a ceiling on how fast pending ceremonies can be begun.
    OidcDevice,
    /// Requests from one federated peer server, across the sync and blob reads (
    /// invariant 21).
    ///
    /// Keyed on the peer's origin, never on the capability: a peer holding ten capabilities is
    /// one blast-radius boundary, and a budget per token would be a budget a peer widens by
    /// asking for more tokens. Events per hour only; bytes and CPU per hour need a weighted
    /// counter this port does not have and are post-v1.
    PeerRequests(String),
    /// Federated moderation reports from one peer against one account (invariant
    /// 24), keyed on `"{reporting_server}:{reported_user}"` as the contract bounds them.
    FederatedReports(String),
    /// Federated moderation reports from one peer against **every** account.
    ///
    /// The ceiling the per-account budget cannot provide: `reported_user` is a string a peer
    /// chooses, so a peer cycling accounts gets a fresh per-account allowance each time, and
    /// only a key that ignores the account bounds the peer's total volume.
    PeerReports(String),
    /// Attempts at `POST /v1/federation/reports`, keyed on the **claimed** reporting origin.
    ///
    /// Charged before the peer is looked up, which is the only place a bound can sit on this
    /// route: it is the server's one unauthenticated write, and everything after it — a store
    /// read and an Ed25519 verification — is work an anonymous caller would otherwise get for
    /// free. The key is attacker-chosen and that is stated rather than papered over: it bounds
    /// one claimed origin looping, not a caller cycling origins, and this server has no trusted
    /// client address to key on instead (see
    /// [`CounterKey::RegistrationSource`], which waits on the same missing fact).
    FederatedIntake(String),
}

/// Which of the two spellings an enrollment code was presented in.
///
/// The redemption limiter's key ([`CounterKey::EnrollmentRedemption`]). Two spellings, two
/// buckets, so a flood of guesses at the short form cannot spend the QR payload's budget: a
/// person whose typed fallback is throttled can still scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnrollmentSpelling {
    /// The full-entropy form the QR payload carries.
    Full,
    /// The shorter transcribable numeric fallback.
    Fallback,
}

impl EnrollmentSpelling {
    /// The scope segment the counter key carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Fallback => "fallback",
        }
    }
}

/// The scope segment a key with nothing to be scoped *to* carries.
///
/// Only the unit variants use it: a counter that partitions by account or by address puts that
/// value here, and one that is a single global bucket has no such value to put. Named rather
/// than written inline at each arm so the two spellings cannot drift.
const GLOBAL_SCOPE: &str = "global";

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
            Self::UploadSessions(_) => "upload_sessions",
            Self::UploadChunks(_) => "upload_chunks",
            Self::UploadSessionsSource(_) => "upload_sessions_source",
            Self::UploadChunksSource(_) => "upload_chunks_source",
            Self::SecondFactor(_) => "second_factor",
            Self::RegistrationSource(_) => "registration_source",
            Self::OidcAuthorize(_) => "oidc_authorize",
            Self::OidcAuthorizeRefused => "oidc_authorize_refused",
            Self::OidcDevice => "oidc_device",
            Self::PeerRequests(_) => "peer_requests",
            Self::FederatedReports(_) => "federated_reports",
            Self::PeerReports(_) => "peer_reports",
            Self::FederatedIntake(_) => "federated_intake",
        }
    }

    /// Which of [`InMemoryCounters`]' [`PARTITION_COUNT`] partitions this key lives in.
    ///
    /// One slot per variant, exactly as [`Self::as_str`] is one name per variant, so a key
    /// never spans two partitions and its charge-and-decide stays under one lock. An exhaustive
    /// match rather than a lookup by name, so a new variant cannot compile without a partition,
    /// and a [`Slot`] rather than a number, so it cannot name a partition the store lacks.
    fn partition_slot(&self) -> Slot {
        match self {
            Self::LoginAttempts(_) => Slot::LoginAttempts,
            Self::EnrollmentRedemption(_) => Slot::EnrollmentRedemption,
            Self::EnrollmentRedemptionMalformed => Slot::EnrollmentRedemptionMalformed,
            Self::ShareLink(_) => Slot::ShareLink,
            Self::ShareSource(_) => Slot::ShareSource,
            Self::DropLink(_) => Slot::DropLink,
            Self::DropSource(_) => Slot::DropSource,
            Self::DeepVerify(_) => Slot::DeepVerify,
            Self::UploadSessions(_) => Slot::UploadSessions,
            Self::UploadChunks(_) => Slot::UploadChunks,
            Self::UploadSessionsSource(_) => Slot::UploadSessionsSource,
            Self::UploadChunksSource(_) => Slot::UploadChunksSource,
            Self::SecondFactor(_) => Slot::SecondFactor,
            Self::RegistrationSource(_) => Slot::RegistrationSource,
            Self::OidcAuthorize(_) => Slot::OidcAuthorize,
            Self::OidcAuthorizeRefused => Slot::OidcAuthorizeRefused,
            Self::OidcDevice => Slot::OidcDevice,
            Self::PeerRequests(_) => Slot::PeerRequests,
            Self::FederatedReports(_) => Slot::FederatedReports,
            Self::PeerReports(_) => Slot::PeerReports,
            Self::FederatedIntake(_) => Slot::FederatedIntake,
        }
    }

    /// The array index of [`Self::partition_slot`], below [`PARTITION_COUNT`] by construction.
    fn partition_index(&self) -> usize {
        self.partition_slot() as usize
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
            Self::UploadSessionsSource(_) => ceilings::UPLOAD_SESSIONS_SOURCE,
            Self::UploadChunksSource(_) => ceilings::UPLOAD_CHUNKS_SOURCE,
            Self::DeepVerify(_) => ceilings::DEEP_VERIFY,
            Self::UploadSessions(_) | Self::UploadChunks(_) => ceilings::UPLOADING_ACCOUNT,
            Self::SecondFactor(_) => ceilings::SECOND_FACTOR,
            Self::OidcAuthorize(_) => ceilings::OIDC_AUTHORIZE,
            Self::OidcAuthorizeRefused => ceilings::OIDC_AUTHORIZE_REFUSED,
            Self::OidcDevice => ceilings::OIDC_DEVICE,
            Self::PeerRequests(_) | Self::PeerReports(_) => ceilings::PEER_ORIGIN,
            Self::FederatedReports(_) => ceilings::FEDERATED_REPORTS,
            Self::FederatedIntake(_) => ceilings::FEDERATED_INTAKE,
        }
    }

    /// What the key is scoped to — the account, the link, the address — for the backend key
    /// that carries it. Paired with [`Self::as_str`], which names the kind.
    pub fn scope(&self) -> &str {
        match self {
            Self::LoginAttempts(user)
            | Self::DeepVerify(user)
            | Self::UploadSessions(user)
            | Self::UploadChunks(user) => user.as_str(),
            Self::EnrollmentRedemption(spelling) => spelling.as_str(),
            Self::ShareLink(scope)
            | Self::ShareSource(scope)
            | Self::DropLink(scope)
            | Self::DropSource(scope)
            | Self::UploadSessionsSource(scope)
            | Self::UploadChunksSource(scope)
            | Self::SecondFactor(scope)
            | Self::OidcAuthorize(scope)
            | Self::PeerRequests(scope)
            | Self::FederatedReports(scope)
            | Self::PeerReports(scope)
            | Self::FederatedIntake(scope)
            | Self::RegistrationSource(scope) => scope,
            // The two unit variants are each **one** global bucket by construction — that is why
            // they are unit variants rather than a scoped one carrying a sentinel string (see
            // their own docs). They still need a scope segment, because the backend key is
            // `capsule:counter:{kind}:{scope}` and a key ending in `:` invites a second, subtly
            // different spelling later. `GLOBAL` is that segment, shared deliberately: `as_str`
            // already distinguishes the two kinds, so one constant cannot collide them.
            Self::EnrollmentRedemptionMalformed | Self::OidcAuthorizeRefused | Self::OidcDevice => {
                GLOBAL_SCOPE
            }
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
    /// adapter owes this atomically; the in-memory one gets it from a mutex and Valkey from one
    /// Lua script that opens, charges or refuses the window in a single server-side step
    /// ([`valkey::ValkeyCounters`]).
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

    /// [`CounterKey::EnrollmentRedemption`](super::CounterKey::EnrollmentRedemption) — keyed on
    /// one of the two [`EnrollmentSpelling`](super::EnrollmentSpelling)s, so two windows.
    ///
    /// It used to be five thousand, sized for a key built from the presented code; issue #528
    /// took the value out of the key, and with it the only way this partition could grow.
    pub const ENROLLMENT_REDEMPTION: usize = 2;

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

    /// [`CounterKey::UploadSessions`](super::CounterKey::UploadSessions) and
    /// [`CounterKey::UploadChunks`](super::CounterKey::UploadChunks) — 1-minute windows, keyed on
    /// an authenticated account.
    ///
    /// Bounded by the directory, as `LOGIN_ATTEMPTS` is: only an account that uploaded in the
    /// last minute holds a window, and no caller can mint a key without an account. Each variant
    /// is its own partition at this size.
    pub const UPLOADING_ACCOUNT: usize = 10_000;

    /// The share, drop and registration source-address keys — 1-minute and 1-hour windows, keyed
    /// on a client address. The two upload source keys are sized below.
    ///
    /// Charged by the share and drop paths (#704); registration's is not yet charged. Sized for
    /// distinct addresses in the window, which for a self-hosted deployment is thousands, not
    /// millions — an IPv6 client counts once per `/64`, so walking a prefix within one `/64` does
    /// not grow it. Each variant is its own partition at this size.
    pub const SOURCE_ADDRESS: usize = 10_000;

    /// How many accounts the two upload source partitions absorb at once, each walking a fresh
    /// source address on every request at its full per-account budget, while still holding
    /// [`SOURCE_ADDRESS`] first-time addresses for everybody else.
    ///
    /// The upload source keys are the one source partition an authenticated caller can grow
    /// faster than addresses arrive: the account budget is charged first, so an account mints at
    /// most its own budget's worth of source keys a minute, but with a routed IPv6 prefix every
    /// one of them can be a distinct `/64`. No ceiling sized against the directory survives every
    /// account doing that, so this number is the stated assumption instead: sixteen accounts
    /// turned hostile at once. Accounts on a self-hosted server are operator-issued, and every
    /// one of those requests is logged under its account before it reaches the source key, so
    /// past this the exposure is accepted: legitimate uploaders whose address holds no live
    /// window are answered `429` with a `Retry-After` of at most one window (one minute) until
    /// the flood stops or the operator disables the accounts. The refusal fails closed and loses
    /// nothing; a client waits out the `Retry-After` and resumes.
    pub const HOSTILE_UPLOADERS: usize = 16;

    /// [`CounterKey::UploadSessionsSource`](super::CounterKey::UploadSessionsSource) — 1-minute
    /// window, keyed on a client address.
    ///
    /// [`budgets::UPLOAD_SESSIONS`](super::budgets::UPLOAD_SESSIONS) × [`HOSTILE_UPLOADERS`] +
    /// [`SOURCE_ADDRESS`] = 300 × 16 + 10 000 = 14 800: the hostile accounts' worth of fresh
    /// addresses on top of the legitimate address population every other source key is sized
    /// for. Derived rather than written, so raising the per-account budget raises this with it.
    // A `u32` widened into `usize`, which is at least 32 bits on every target this server builds.
    pub const UPLOAD_SESSIONS_SOURCE: usize =
        super::budgets::UPLOAD_SESSIONS.limit as usize * HOSTILE_UPLOADERS + SOURCE_ADDRESS;

    /// [`CounterKey::UploadChunksSource`](super::CounterKey::UploadChunksSource) — 1-minute
    /// window, keyed on a client address.
    ///
    /// [`budgets::UPLOAD_CHUNKS`](super::budgets::UPLOAD_CHUNKS) × [`HOSTILE_UPLOADERS`] +
    /// [`SOURCE_ADDRESS`] = 3 000 × 16 + 10 000 = 58 000, as [`UPLOAD_SESSIONS_SOURCE`] is
    /// derived. Under [`SOURCE_ADDRESS`] alone this partition filled at four such accounts.
    pub const UPLOAD_CHUNKS_SOURCE: usize =
        super::budgets::UPLOAD_CHUNKS.limit as usize * HOSTILE_UPLOADERS + SOURCE_ADDRESS;

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

    /// [`CounterKey::OidcDevice`](super::CounterKey::OidcDevice) — one key exists, so one
    /// window.
    pub const OIDC_DEVICE: usize = 1;

    /// [`CounterKey::PeerRequests`](super::CounterKey::PeerRequests) and
    /// [`CounterKey::PeerReports`](super::CounterKey::PeerReports) — both keyed on a peer's
    /// origin, so both are bounded by the same thing and share one number.
    ///
    /// Not caller-controlled: a peer origin reaches either key only off a capability this
    /// server minted, so the true bound is the size of the peer list an operator pinned. Ten
    /// thousand is the same rounding as [`LOGIN_ATTEMPTS`], and for the same reason — a
    /// deployment federating with more peers than that has other numbers to raise first.
    pub const PEER_ORIGIN: usize = 10_000;

    /// [`CounterKey::FederatedReports`](super::CounterKey::FederatedReports) — 1-hour window,
    /// keyed on `"{reporting_server}:{reported_user}"`.
    ///
    /// The account half is a string a peer chooses, so this partition *is* growable by a
    /// misbehaving peer — which is exactly why
    /// [`CounterKey::PeerReports`](super::CounterKey::PeerReports) exists. That is also what
    /// bounds this: a report charges both keys, so a peer cannot create more distinct pairs per
    /// hour than [`budgets::PEER_REPORTS`](super::budgets::PEER_REPORTS) admits — 200. Across a
    /// pinned peer list of the order [`PEER_ORIGIN`] anticipates, twenty thousand is roughly a
    /// hundred peers each spending their whole hourly allowance on distinct accounts.
    pub const FEDERATED_REPORTS: usize = 20_000;

    /// [`CounterKey::FederatedIntake`](super::CounterKey::FederatedIntake) — 1-hour window,
    /// keyed on the **claimed** reporting origin.
    ///
    /// The one federation key charged before anything is authenticated, so the key space is
    /// attacker-chosen outright and this ceiling is the only bound on it. Sized like
    /// [`SHARE_LINK`] and [`DROP_LINK`], the other partitions a caller supplies the key for:
    /// twenty thousand distinct claimed origins in an hour is under six a second, and spending
    /// it costs the intake path its first-time keys and costs no other surface anything. That
    /// containment is the whole reason it is a partition rather than a share of one.
    pub const FEDERATED_INTAKE: usize = 20_000;
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
    /// One lock per [`CounterKey`] variant, so one variant's occupancy is invisible to
    /// another's ceiling and one variant's traffic never waits on another's lock. The array
    /// itself is fixed at construction and read without a lock.
    windows: Partitions,
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

/// Declares [`Slot`] and derives [`PARTITION_COUNT`] from the same list, so the two cannot
/// disagree: adding a slot grows the array, and no slot can index past it.
macro_rules! partition_slots {
    ($($slot:ident),+ $(,)?) => {
        /// One partition of [`InMemoryCounters`], named for the [`CounterKey`] variant it holds.
        ///
        /// Fieldless with implicit discriminants, so `slot as usize` runs `0..PARTITION_COUNT`
        /// and indexing the partition array with it cannot go out of bounds.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(usize)]
        enum Slot {
            $($slot),+
        }

        /// How many partitions the store holds: one per [`Slot`], counted from its declaration.
        const PARTITION_COUNT: usize = [$(Slot::$slot),+].len();
    };
}

partition_slots!(
    LoginAttempts,
    EnrollmentRedemption,
    EnrollmentRedemptionMalformed,
    ShareLink,
    ShareSource,
    DropLink,
    DropSource,
    DeepVerify,
    UploadSessions,
    UploadChunks,
    UploadSessionsSource,
    UploadChunksSource,
    SecondFactor,
    RegistrationSource,
    OidcAuthorize,
    OidcAuthorizeRefused,
    OidcDevice,
    PeerRequests,
    FederatedReports,
    PeerReports,
    FederatedIntake,
);

/// One variant's windows, behind that variant's own lock.
type Partition = Mutex<BTreeMap<CounterKey, Window>>;

/// The partitioned window map: one independently locked map per [`CounterKey`] variant,
/// indexed by [`CounterKey::partition_index`].
type Partitions = [Partition; PARTITION_COUNT];

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
    ///
    /// Each partition is counted under its own lock in turn, so under concurrent writes the sum
    /// is not one snapshot of the whole store.
    pub fn len(&self) -> usize {
        self.windows
            .iter()
            .map(|partition| lock(partition).len())
            .sum()
    }

    /// How many distinct keys hold a window in `key`'s partition.
    ///
    /// The number the ceiling is actually compared against, so a test can assert that filling
    /// one surface left another's occupancy alone.
    pub fn len_of(&self, key: &CounterKey) -> usize {
        lock(self.partition(key)).len()
    }

    /// Whether no key holds a window.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// This key's partition ceiling, or the override every partition shares when one is set.
    fn ceiling_for(&self, key: &CounterKey) -> usize {
        self.ceiling_override.unwrap_or_else(|| key.ceiling())
    }

    /// `key`'s partition. Infallible: [`CounterKey::partition_index`] is a [`Slot`]'s
    /// discriminant, and [`PARTITION_COUNT`] is counted from the same declaration.
    fn partition(&self, key: &CounterKey) -> &Partition {
        &self.windows[key.partition_index()]
    }

    /// Drop every window in `windows` whose budget has lapsed.
    fn purge(windows: &mut BTreeMap<CounterKey, Window>, now: Timestamp) {
        windows.retain(|_, window| now < window.purge_after);
    }

    /// Purge every partition except `own` that nobody else holds right now.
    ///
    /// Opportunistic, so a write still collects what lapsed in a partition nobody writes to,
    /// without ever waiting on another partition's lock: a partition another caller holds is
    /// skipped, and that caller purges it on its own write. One lock at a time, and never while
    /// `own`'s is held, so no ordering between partition locks exists to deadlock on.
    fn purge_others(&self, own: usize, now: Timestamp) {
        for (index, partition) in self.windows.iter().enumerate() {
            if index == own {
                continue;
            }
            let mut guard = match partition.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => continue,
            };
            Self::purge(&mut guard, now);
        }
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
            let own = key.partition_index();
            let partition = self.partition(key);
            self.purge_others(own, at);

            // Everything from here is under this key's partition lock alone: the charge and the
            // decision stay one critical section, and no other variant's caller waits on it.
            let mut windows = lock(partition);
            Self::purge(&mut windows, at);

            let held = windows.get(key).copied();
            let ceiling = self.ceiling_for(key);
            let occupancy = windows.len();
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
            let partition = self.partition(key);
            let held = lock(partition).get(key).copied();
            Ok(verdict(held, budget, at).0)
        })
    }

    fn reset<'a>(&'a self, key: &'a CounterKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let partition = self.partition(key);
            if lock(partition).remove(key).is_some() {
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
pub mod conformance;
pub mod valkey;

#[cfg(test)]
mod tests;
