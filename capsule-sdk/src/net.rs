//! Connection-class and retry-policy contract types (SSoT: the Network
//! Resilience design doc). Detection (OS signals + behavioral promotion to
//! [`ConnectionClass::Adverse`]) and the per-class retry engines
//! live here: class detection feeds the sync and cache budgets, and the adverse class hardens
//! the network behaviour.
//!
//! This module fills the `capsule-sdk::net` seam named by the networking doc:
//! it maps **mocked OS path signals** ([`PathSignals`]) to a base
//! [`ConnectionClass`], layers the behavioral **`adverse` promotion/demotion**
//! ([`AdverseDetector`]) on top, and exposes the two gate surfaces the class
//! feeds — the **staged-upload tier gates** ([`ConnectionClass::permits_tier`],
//! over the canonical [`UploadTier`]) and the **cache-eviction byte budget**
//! ([`ConnectionClass::cache_retention_budget`], which [`crate::cache_budget`] feeds to the
//! eviction sweep). Detection is signal-driven,
//! never a live NIC probe, so every rule is a deterministic unit test.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use capsule_library::import::UploadTier;
use tracing::instrument;

pub mod negotiation;

/// The closed connection-class enum, evaluated continuously on-device.
/// Consumers: sync criteria (small/large reconciliation), staged-upload tier
/// gates, the cache-eviction byte budget, prefetch, adaptive chunk sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionClass {
    /// Bulk transfer is acceptable.
    Unmetered,
    /// Byte-counted link; bulk work is deferred.
    Metered,
    /// OS-level data saver (iOS Low Data Mode / Android Data Saver).
    Constrained,
    /// Connectivity present but unreliable — behavioral promotion on repeated
    /// mid-transfer resets/stalls within a sliding window; demoted after a
    /// clean period. No OS reports this class; it is derived.
    Adverse,
    /// No usable path.
    Offline,
}

/// The mocked OS path signals a platform samples to classify the current link.
///
/// Named after the platform detection inputs in the networking doc's
/// connection-class table: iOS `NWPathMonitor` (`isExpensive`, `isConstrained`),
/// Android `NET_CAPABILITY_NOT_METERED` / Data Saver, desktop default. Tests
/// drive [`ConnectionClass::from_signals`] with these directly — there is no live
/// NIC probing anywhere in the crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSignals {
    /// The OS reports a usable network path at all (`false` ⇒ `offline`).
    pub has_path: bool,
    /// iOS `isExpensive` / a metered cellular capability is present.
    pub is_expensive: bool,
    /// Android `NET_CAPABILITY_NOT_METERED` is present (an explicitly unmetered
    /// link). Absent metered capability alone is treated as metered.
    pub not_metered_capability: bool,
    /// OS-level data saver is active (iOS Low Data Mode / Android Data Saver).
    pub is_constrained: bool,
}

impl PathSignals {
    /// The desktop / wired default: a usable, unmetered, unconstrained path.
    #[must_use]
    pub fn unmetered() -> Self {
        Self {
            has_path: true,
            is_expensive: false,
            not_metered_capability: true,
            is_constrained: false,
        }
    }

    /// No usable path at all.
    #[must_use]
    pub fn offline() -> Self {
        Self {
            has_path: false,
            is_expensive: true,
            not_metered_capability: false,
            is_constrained: false,
        }
    }
}

impl ConnectionClass {
    /// Map sampled OS signals to the **base** class, before any behavioral
    /// promotion to [`ConnectionClass::Adverse`] (which no OS reports — see
    /// [`AdverseDetector`]).
    ///
    /// Precedence is deliberate and total: no path ⇒ `offline`; else data saver
    /// ⇒ `constrained` (the strongest user "minimize data" signal wins over a
    /// merely metered link); else an expensive / not-explicitly-unmetered link ⇒
    /// `metered`; else `unmetered`. `constrained` and `metered` are always
    /// distinguished (networking doc Validation: "class detection").
    #[must_use]
    pub fn from_signals(signals: PathSignals) -> Self {
        if !signals.has_path {
            Self::Offline
        } else if signals.is_constrained {
            Self::Constrained
        } else if signals.is_expensive || !signals.not_metered_capability {
            Self::Metered
        } else {
            Self::Unmetered
        }
    }

    /// Whether any usable path exists (every class but [`ConnectionClass::Offline`]).
    #[must_use]
    pub fn is_usable(self) -> bool {
        self != Self::Offline
    }

    /// Whether the link is treated as **non-metered** for tier and reconciliation
    /// gating.
    ///
    /// Only [`ConnectionClass::Unmetered`] qualifies: its definition is precisely
    /// "bulk transfer is acceptable". [`ConnectionClass::Metered`] (byte-counted)
    /// and [`ConnectionClass::Constrained`] (data saver) are byte-conscious, and
    /// [`ConnectionClass::Adverse`] is unreliable — so above-index tiers defer on
    /// all three, matching the tier ladder's "even constrained/adverse" T0 row and
    /// the adverse posture's "the staged-upload T0 index still escapes over the
    /// thin link".
    #[must_use]
    pub fn is_non_metered(self) -> bool {
        self == Self::Unmetered
    }

    /// Whether a staged-upload tier may open on this connection.
    ///
    /// Mirrors the download/upload tier ladder (transfer upload doc, "Upload
    /// Tiering"): T0 index escapes on any usable connection (a few KB per asset);
    /// T1 preview and T2 original need a non-metered link — or an explicit
    /// user-consented `force_sync`, which overrides the metered/Wi-Fi criteria but
    /// never resurrects an offline path.
    #[must_use]
    pub fn permits_tier(self, tier: UploadTier, force_sync: bool) -> bool {
        match tier {
            UploadTier::Index => self.is_usable(),
            UploadTier::Preview | UploadTier::Original => {
                self.is_usable() && (self.is_non_metered() || force_sync)
            }
        }
    }

    /// Whether a reconciliation of the given size may run now (sync criteria:
    /// small vs. large). Small (a handful of assets / metadata deltas) and large
    /// (bulk originals) both require a non-metered link; `force_sync` is the
    /// two-week-staleness "force sync now" consent that overrides it. The extra
    /// deferral of a large reconciliation (idle/on-power scheduling) is the
    /// scheduler's concern, layered above this connection gate.
    #[must_use]
    pub fn permits_reconciliation(self, class: ReconciliationClass, force_sync: bool) -> bool {
        let _ = class;
        self.is_usable() && (self.is_non_metered() || force_sync)
    }

    /// The cache-eviction **byte budget** for this class, from a base budget.
    ///
    /// The budget is the ceiling of cache bytes the client retains before the
    /// last-access eviction sweep (owned by Filesystem — Client) reclaims space.
    /// It scales inversely with re-fetch cost: on an [`ConnectionClass::Unmetered`]
    /// link re-fetching an evicted blob is cheap, so the client keeps the tight
    /// base budget and evicts freely; on byte-conscious, unreliable, or offline
    /// links re-fetch is expensive or impossible, so it retains more. The
    /// multipliers are client-tunable policy, not protocol surface; only their
    /// monotonicity (`unmetered ≤ metered/constrained ≤ adverse ≤ offline`) is a
    /// contract.
    #[must_use]
    pub fn cache_retention_budget(self, base_budget_bytes: u64) -> u64 {
        let multiplier = match self {
            Self::Unmetered => 1,
            Self::Metered | Self::Constrained => 2,
            Self::Adverse => 3,
            Self::Offline => 4,
        };
        base_budget_bytes.saturating_mul(multiplier)
    }

    /// The **bounded `Range` window** a blob fetch uses on this class, or `None`
    /// for the open-ended remainder.
    ///
    /// Adverse-network posture (networking doc, "Bounded transfer windows under
    /// `adverse`"): only [`ConnectionClass::Adverse`] shrinks fetches to a bounded
    /// window ([`ADVERSE_RANGE_WINDOW`]) so each request is small enough to usually
    /// complete between mid-transfer resets; every other class fetches the whole
    /// remaining range in one request (`None`). The window size is client-tunable
    /// policy, not protocol surface.
    #[must_use]
    pub fn range_window(self) -> Option<u64> {
        match self {
            Self::Adverse => Some(ADVERSE_RANGE_WINDOW),
            _ => None,
        }
    }
}

/// The bounded `Range` window (256 KiB) a blob fetch shrinks to under
/// [`ConnectionClass::Adverse`]. Small enough that a single window usually
/// completes between the mid-transfer resets that define an adverse path;
/// client-tunable policy, not protocol surface.
pub const ADVERSE_RANGE_WINDOW: u64 = 256 * 1024;

/// The size class of a pending reconciliation (sync criteria). Scales with the
/// total upload + download transfer amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationClass {
    /// A handful of new assets, or metadata-only deltas.
    Small,
    /// Bulk uploads, or original-tier downloads (also a storage-constrained
    /// streaming import).
    Large,
}

/// An observed transfer outcome, fed to the [`AdverseDetector`]. A reset, stall,
/// or black-hole counts toward `adverse`; a clean completion counts toward
/// demotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferSignal {
    /// The transfer chunk/window completed cleanly.
    Clean,
    /// The connection reset mid-transfer.
    Reset,
    /// A stall (no bytes for the stall bound) cut the transfer.
    Stall,
    /// The request was silently black-holed (no response, no error).
    BlackHole,
}

impl TransferSignal {
    /// Whether this outcome counts toward `adverse` promotion.
    #[must_use]
    fn is_adverse(self) -> bool {
        !matches!(self, Self::Clean)
    }
}

/// Tunable thresholds for behavioral [`ConnectionClass::Adverse`] promotion. Not
/// protocol surface — a client picks values to match its network posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdverseThresholds {
    /// Size of the sliding window (most-recent transfer outcomes considered).
    pub window: usize,
    /// Adverse outcomes within the window that promote to `adverse`.
    pub promote_at: usize,
    /// Consecutive clean outcomes that demote back out of `adverse`.
    pub demote_after_clean: usize,
}

impl Default for AdverseThresholds {
    fn default() -> Self {
        // Conservative defaults: three bad outcomes in the last eight promote;
        // four clean transfers in a row demote.
        Self {
            window: 8,
            promote_at: 3,
            demote_after_clean: 4,
        }
    }
}

/// The behavioral `adverse` detector: a deterministic sliding-window state
/// machine over recent transfer outcomes.
///
/// No OS reports `adverse` — the networks that need it most look "connected" to
/// every OS API — so it is *derived*: `≥ promote_at` reset/stall/black-hole
/// outcomes within the last `window` transfers promote the link to `adverse`
/// (which bounds fetch windows and pins staged uploads to the index tier), and a
/// run of `demote_after_clean` clean transfers demotes it back. Being event-
/// driven rather than clock-driven, it is fully deterministic under test.
#[derive(Debug, Clone)]
pub struct AdverseDetector {
    thresholds: AdverseThresholds,
    recent: VecDeque<TransferSignal>,
    consecutive_clean: usize,
    adverse: bool,
}

impl AdverseDetector {
    /// A fresh detector with the given thresholds, starting non-adverse.
    #[must_use]
    pub fn new(thresholds: AdverseThresholds) -> Self {
        Self {
            thresholds,
            recent: VecDeque::with_capacity(thresholds.window.max(1)),
            consecutive_clean: 0,
            adverse: false,
        }
    }

    /// A detector with the [default thresholds](AdverseThresholds::default).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(AdverseThresholds::default())
    }

    /// Fold one transfer outcome into the window and recompute the state.
    pub fn record(&mut self, signal: TransferSignal) {
        if self.recent.len() == self.thresholds.window {
            self.recent.pop_front();
        }
        self.recent.push_back(signal);

        if signal.is_adverse() {
            self.consecutive_clean = 0;
        } else {
            self.consecutive_clean += 1;
        }

        if self.adverse {
            // Demote only after a sustained clean run.
            if self.consecutive_clean >= self.thresholds.demote_after_clean {
                self.adverse = false;
                self.recent.clear();
            }
        } else {
            let adverse_in_window = self.recent.iter().filter(|s| s.is_adverse()).count();
            if adverse_in_window >= self.thresholds.promote_at {
                self.adverse = true;
                self.consecutive_clean = 0;
            }
        }
    }

    /// Whether the link is currently promoted to `adverse`.
    #[must_use]
    pub fn is_adverse(&self) -> bool {
        self.adverse
    }

    /// The effective class: `offline` is never overridden; any other base class is
    /// promoted to [`ConnectionClass::Adverse`] while the detector is tripped.
    #[must_use]
    pub fn classify(&self, base: ConnectionClass) -> ConnectionClass {
        if base == ConnectionClass::Offline || !self.adverse {
            base
        } else {
            ConnectionClass::Adverse
        }
    }
}

/// The three retry policy classes every retrying surface instantiates.
/// Universal rules (owned by the Network Resilience doc): exponential backoff
/// with full jitter, honor server backoff signals, bounded give-up surfaced to
/// the user, never hot-loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    /// Short timeout, ≤ 2 retries, then a visible failure state.
    Interactive,
    /// Resume-first (HEAD offset / Range), patient within the session lifetime.
    BulkTransfer,
    /// Slow ladder, long horizon, never abandons silently (MLS recovery,
    /// federation circuit breaker are instances).
    ControlCeremony,
}

impl RetryClass {
    /// The canonical [`RetryPolicy`] for this class (networking doc, "Retry Policy
    /// Classes"). The three per-doc retry ladders are *instances of this shared
    /// shape*, not reinventions — sync, upload, and fetch each build a
    /// [`RetryEngine`] from one of these.
    #[must_use]
    pub fn policy(self) -> RetryPolicy {
        match self {
            // Short timeout, ≤ 2 retries, then a visible failure state.
            Self::Interactive => RetryPolicy {
                max_retries: 2,
                base_delay: Duration::from_millis(200),
                max_delay: Duration::from_secs(2),
            },
            // Resume-first and patient within the session's lifetime.
            Self::BulkTransfer => RetryPolicy {
                max_retries: 8,
                base_delay: Duration::from_millis(500),
                max_delay: Duration::from_secs(30),
            },
            // Slow ladder, long horizon, never abandons silently.
            Self::ControlCeremony => RetryPolicy {
                max_retries: 12,
                base_delay: Duration::from_secs(30),
                max_delay: Duration::from_secs(600),
            },
        }
    }

    /// A fresh [`RetryEngine`] instance for this class, with production full-jitter
    /// seeded from the wall clock. The single constructor sync/upload/fetch call.
    #[must_use]
    pub fn engine(self) -> RetryEngine {
        RetryEngine::new(self)
    }
}

// ─── Shared retry engine ─────────────────────────────────────────────────────

/// The concrete parameters of a [`RetryClass`]: a bounded retry count and the
/// exponential-backoff envelope. `max_retries` is finite by type, so no policy —
/// and therefore no surface instantiating one — can ever hot-loop (networking doc:
/// "every retry loop has a bounded give-up"; "no surface ever hot-loops").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts after the first failure before a visible give-up.
    pub max_retries: u32,
    /// The first backoff ceiling; doubles each attempt up to `max_delay`.
    pub base_delay: Duration,
    /// The backoff ceiling cap (the doubling never exceeds this).
    pub max_delay: Duration,
}

/// The engine's decision after one failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Wait `after` (already jittered and reconciled with any server hint), then
    /// retry.
    Retry {
        /// The reconciled backoff for this attempt.
        after: Duration,
    },
    /// The bounded retry budget is spent — surface a user-visible failure state.
    GiveUp,
}

/// The randomness source for **full jitter**. Production draws a spread value in
/// `[0, 1)`; tests pin a fixed fraction so the whole backoff sequence is
/// deterministic.
#[derive(Debug, Clone)]
enum Jitter {
    /// An `xorshift64*` state, advanced per draw (fast, dependency-free spread).
    Random(u64),
    /// A fixed fraction in `[0, 1]`, for deterministic tests.
    Fixed(f64),
}

impl Jitter {
    /// The next jitter fraction in `[0, 1)` (or the pinned fixed value).
    fn next_fraction(&mut self) -> f64 {
        match self {
            Self::Random(state) => {
                // xorshift64*: cheap, well-distributed, and enough for jitter
                // (this is spread, not security). Kept nonzero at construction.
                let mut x = *state;
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                *state = x;
                let v = x.wrapping_mul(0x2545_f491_4f6c_dd1d);
                // Top 53 bits → a double in [0, 1).
                (v >> 11) as f64 / ((1u64 << 53) as f64)
            }
            Self::Fixed(fraction) => *fraction,
        }
    }
}

/// The one shared retry engine every retrying surface instantiates
/// ([`RetryClass::engine`]). It owns the universal rules so no surface re-derives
/// them: **exponential backoff with full jitter**, honoring a server backoff
/// signal (`Retry-After` / 429 / 503) as a floor, and a **bounded give-up** after
/// `max_retries` — never an unbounded hot loop.
///
/// The engine is a pure state machine over the attempt counter and the jitter
/// source: [`RetryEngine::next_backoff`] computes the wait and the give-up without
/// sleeping, so backoff discipline is a deterministic unit test. Callers perform
/// the wait through a [`Sleeper`] (real in production, a no-op recorder in tests).
#[derive(Debug, Clone)]
pub struct RetryEngine {
    class: RetryClass,
    policy: RetryPolicy,
    attempt: u32,
    jitter: Jitter,
}

impl RetryEngine {
    /// A production engine for `class`, full-jitter seeded from the wall clock.
    #[must_use]
    pub fn new(class: RetryClass) -> Self {
        Self::with_jitter(class, Jitter::Random(seed()))
    }

    /// A deterministic engine that always jitters by the fixed `fraction`
    /// (clamped to `[0, 1]`) — for tests that assert exact backoff values.
    #[must_use]
    pub fn deterministic(class: RetryClass, fraction: f64) -> Self {
        Self::with_jitter(class, Jitter::Fixed(fraction.clamp(0.0, 1.0)))
    }

    /// A production-shaped engine (full jitter) with a fixed seed — for tests that
    /// assert the *spread* property of full jitter deterministically.
    #[must_use]
    pub fn seeded(class: RetryClass, seed: u64) -> Self {
        Self::with_jitter(class, Jitter::Random(seed | 1))
    }

    fn with_jitter(class: RetryClass, jitter: Jitter) -> Self {
        Self {
            class,
            policy: class.policy(),
            attempt: 0,
            jitter,
        }
    }

    /// The class this engine instantiates.
    #[must_use]
    pub fn class(&self) -> RetryClass {
        self.class
    }

    /// The policy bounds this engine enforces.
    #[must_use]
    pub fn policy(&self) -> RetryPolicy {
        self.policy
    }

    /// Failures observed so far (retries consumed).
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    /// Reset the attempt counter after real progress, so a long-lived transfer's
    /// backoff does not ratchet across independent stalls (resume-first: each
    /// resumed window earns a fresh budget).
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// The un-jittered backoff ceiling for the *next* attempt: `base · 2^attempt`,
    /// saturating and capped at `max_delay`.
    fn ceiling(&self) -> Duration {
        let base_ms = self.policy.base_delay.as_millis() as u64;
        let cap_ms = self.policy.max_delay.as_millis() as u64;
        // Cap the exponent so the shift never overflows before the min() clamps it.
        let exp = self.attempt.min(32);
        let scaled = base_ms.saturating_mul(1u64.checked_shl(exp).unwrap_or(u64::MAX));
        Duration::from_millis(scaled.min(cap_ms))
    }

    /// Decide the next action after a failure, folding in an optional server
    /// backoff hint (`Retry-After` / 429 / 503).
    ///
    /// Returns [`RetryDecision::GiveUp`] once the bounded budget is spent (so no
    /// configuration can hot-loop). Otherwise the wait is **full jitter** over the
    /// ceiling — a uniform draw in `[0, ceiling]` — reconciled with the server hint
    /// as a *floor* (`Retry-After` is honored even when it exceeds the jittered
    /// value). The ceiling itself is bounded by `max_delay`, so the client-chosen
    /// component of the wait never exceeds the policy cap.
    #[instrument(level = "trace", skip(self), fields(class = ?self.class, attempt = self.attempt))]
    pub fn next_backoff(&mut self, server_backoff: Option<Duration>) -> RetryDecision {
        if self.attempt >= self.policy.max_retries {
            tracing::debug!(
                max_retries = self.policy.max_retries,
                "retry budget exhausted — giving up (visible failure state)"
            );
            return RetryDecision::GiveUp;
        }
        let ceiling = self.ceiling();
        self.attempt += 1;
        let fraction = self.jitter.next_fraction().clamp(0.0, 1.0);
        let jittered = ceiling.mul_f64(fraction);
        // The server's backoff signal is a floor we always honor; the jittered
        // value is our own spread. Take the larger.
        let after = match server_backoff {
            Some(hint) => jittered.max(hint),
            None => jittered,
        };
        tracing::trace!(?after, ?ceiling, "backing off before retry");
        RetryDecision::Retry { after }
    }
}

/// A non-cryptographic seed for the jitter PRNG from the wall clock, X's a fixed
/// odd constant and forced nonzero. Only spread matters here.
fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64);
    (nanos ^ 0x9e37_79b9_7f4a_7c15) | 1
}

// ─── Stall detection (no-bytes-for-T) ────────────────────────────────────────

/// A monotonic millisecond clock, injectable so the stall detector's cut is a
/// deterministic test (a mock clock the test advances) rather than a wall-clock
/// sleep.
pub trait MonotonicClock {
    /// Milliseconds since some fixed, monotonic origin.
    fn now_millis(&self) -> u64;
}

/// The production monotonic clock (a process-relative [`Instant`]).
#[derive(Debug, Clone)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// A clock whose origin is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock for SystemClock {
    fn now_millis(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

/// The stall bound: bulk transfers cut on **no-bytes-for-T**, not on a total
/// duration (networking doc, "Stall detection over total timeouts"). Client-tunable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StallConfig {
    /// The no-progress interval that trips a stall cut.
    pub stall_after: Duration,
}

impl StallConfig {
    /// A config with an explicit stall bound.
    #[must_use]
    pub fn new(stall_after: Duration) -> Self {
        Self { stall_after }
    }
}

impl Default for StallConfig {
    fn default() -> Self {
        // Long enough not to punish slow-but-live links; short enough that a
        // silently black-holed request is cut in seconds, not minutes.
        Self {
            stall_after: Duration::from_secs(20),
        }
    }
}

/// The **no-bytes-for-T** stall detector: tracks the last time bytes arrived and
/// reports when the gap since then has crossed the stall bound. Event-driven over
/// an injected [`MonotonicClock`] (never `sleep`), so the "cut within its bound"
/// behavior is fully deterministic under test.
#[derive(Debug, Clone)]
pub struct StallDetector {
    stall_after_millis: u64,
    last_progress_millis: u64,
}

impl StallDetector {
    /// A detector armed at `now_millis` with `config`'s stall bound.
    #[must_use]
    pub fn new(config: StallConfig, now_millis: u64) -> Self {
        Self {
            stall_after_millis: config.stall_after.as_millis() as u64,
            last_progress_millis: now_millis,
        }
    }

    /// Record that bytes arrived at `now_millis`: the no-progress timer resets.
    pub fn on_progress(&mut self, now_millis: u64) {
        self.last_progress_millis = now_millis;
    }

    /// Whether the transfer has gone `stall_after` with no bytes as of `now_millis`
    /// — the cut condition.
    #[must_use]
    pub fn is_stalled(&self, now_millis: u64) -> bool {
        now_millis.saturating_sub(self.last_progress_millis) >= self.stall_after_millis
    }

    /// The configured stall bound.
    #[must_use]
    pub fn stall_after(&self) -> Duration {
        Duration::from_millis(self.stall_after_millis)
    }
}

// ─── Sleeper seam ────────────────────────────────────────────────────────────

/// How a retry/stall loop performs its backoff wait. Abstracted so the wait is
/// real in production ([`TokioSleeper`]) but a deterministic no-op in tests — the
/// stall-cut-resume smoke asserts *zero duplicate bytes* with no wall-clock sleep.
pub trait Sleeper: Clone + Send + Sync {
    /// Wait for `dur`.
    fn sleep(&self, dur: Duration) -> impl std::future::Future<Output = ()> + Send;
}

/// The production sleeper (`tokio::time::sleep`).
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    async fn sleep(&self, dur: Duration) {
        tokio::time::sleep(dur).await;
    }
}

// ─── Per-request stall bounds ────────────────────────────────────────────────

/// How long a request waits for its response once its body is fully sent: one minute.
///
/// Longer than the stall bound, because it covers the server's own work on a write — a chunk's
/// staging write and `fsync` — rather than bytes on the wire (networking doc, Adverse-Network
/// Posture). Waiting on a session whose finalization is still running is not this wait: that is
/// observed by polling `HEAD`, never by holding a request open.
pub const DEFAULT_RESPONSE_WAIT: Duration = Duration::from_mins(1);

/// The size of one piece a [`PacedBody`] hands the HTTP stack: 64 KiB.
///
/// Small enough that a piece pulled is a fresh sign the link is moving, even at a few KiB/s;
/// large enough that the per-piece bookkeeping is noise beside the bytes.
pub const PACED_PIECE: usize = 64 * 1024;

/// The two bounds every request runs under: the **stall** bound while bytes should be moving —
/// sending the request body, or receiving the response body — and the longer **response-wait**
/// bound between the last request byte and the response head.
///
/// Neither is a total-duration timeout: a slow but live request is never cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestBounds {
    /// No bytes moved for this long while sending or receiving cuts the request.
    pub stall: Duration,
    /// No response head this long after the body was sent cuts the request.
    pub response_wait: Duration,
}

impl RequestBounds {
    /// Bounds of `stall` and `response_wait`.
    #[must_use]
    pub const fn new(stall: Duration, response_wait: Duration) -> Self {
        Self {
            stall,
            response_wait,
        }
    }
}

impl Default for RequestBounds {
    /// The [`StallConfig`] default stall bound — below the server's own, so the client usually
    /// notices first — and [`DEFAULT_RESPONSE_WAIT`].
    fn default() -> Self {
        Self::new(StallConfig::default().stall_after, DEFAULT_RESPONSE_WAIT)
    }
}

/// Why a request was cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestCut {
    /// No bytes moved for the stall bound, sending or receiving.
    Stalled,
    /// The body was sent, and no response head came within the response-wait bound.
    NoResponse,
}

impl std::fmt::Display for RequestCut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stalled => "the request stalled: no bytes moved for the stall bound",
            Self::NoResponse => "no response arrived within the response-wait bound",
        })
    }
}

/// Where one request's exchange is, and when it last moved.
#[derive(Debug, Clone, Copy)]
enum Phase {
    /// Sending the body; a piece last left at `last`.
    Sending { last: tokio::time::Instant },
    /// The body is sent; waiting for the head since `since`.
    Waiting { since: tokio::time::Instant },
    /// Receiving the response body; bytes last arrived at `last`.
    Receiving { last: tokio::time::Instant },
}

/// The progress of one request, shared between the body that sends it, the code that reads its
/// response, and [`within_bounds`] that watches both. Cheap to clone: every clone is the same
/// progress.
///
/// Read on tokio's clock, so a paused test clock drives it like every other timer.
#[derive(Debug, Clone)]
pub struct ExchangeProgress {
    phase: std::sync::Arc<std::sync::Mutex<Phase>>,
}

impl ExchangeProgress {
    /// A request with nothing (more) to send, waiting for its response from now.
    #[must_use]
    pub fn waiting() -> Self {
        Self {
            phase: std::sync::Arc::new(std::sync::Mutex::new(Phase::Waiting {
                since: tokio::time::Instant::now(),
            })),
        }
    }

    fn set(&self, phase: Phase) {
        // A poisoned lock holds a phase still worth reading: the watcher only reads instants.
        *self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = phase;
    }

    fn phase(&self) -> Phase {
        *self
            .phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The body started sending, or a piece of it left, now.
    pub fn on_sent(&self) {
        self.set(Phase::Sending {
            last: tokio::time::Instant::now(),
        });
    }

    /// The whole body has been handed off, now: the response wait starts.
    pub fn on_sent_all(&self) {
        self.set(Phase::Waiting {
            since: tokio::time::Instant::now(),
        });
    }

    /// Response bytes arrived — or the head did — now.
    pub fn on_received(&self) {
        self.set(Phase::Receiving {
            last: tokio::time::Instant::now(),
        });
    }

    /// When the exchange is cut unless it moves first, and as what.
    fn deadline(&self, bounds: RequestBounds) -> (tokio::time::Instant, RequestCut) {
        match self.phase() {
            Phase::Sending { last } | Phase::Receiving { last } => {
                (last + bounds.stall, RequestCut::Stalled)
            }
            Phase::Waiting { since } => (since + bounds.response_wait, RequestCut::NoResponse),
        }
    }
}

/// Drive `exchange` while `progress` keeps moving inside `bounds`; cut it when it does not.
///
/// The deadline moves with the progress, so the watcher sleeps to the current deadline, and on
/// waking re-reads it: progress since pushes it out, and none means the cut. Nothing here bounds
/// the exchange's total duration.
///
/// On a cut `exchange` is dropped, and with it the connection: on HTTP/1.1 it closes, on HTTP/2
/// the stream resets (networking doc, HTTP Transport).
///
/// # Errors
///
/// The [`RequestCut`] that ended it.
pub async fn within_bounds<F: std::future::Future>(
    bounds: RequestBounds,
    progress: &ExchangeProgress,
    exchange: F,
) -> Result<F::Output, RequestCut> {
    let mut exchange = std::pin::pin!(exchange);
    loop {
        let (deadline, cut) = progress.deadline(bounds);
        if tokio::time::Instant::now() >= deadline {
            return Err(cut);
        }
        if let Ok(output) = tokio::time::timeout_at(deadline, exchange.as_mut()).await {
            return Ok(output);
        }
    }
}

/// A request body sent in [`PACED_PIECE`] pieces, recording each piece the HTTP stack pulls in
/// an [`ExchangeProgress`].
///
/// The stack pulls the next piece once it has written the one before, so a pull is the link
/// moving and a long gap between pulls is the link stalled. Handing out the last piece starts
/// the response wait. The exact length is advertised, so the request still carries a
/// `Content-Length` and is never sent chunked.
#[derive(Debug)]
pub struct PacedBody {
    remaining: bytes::Bytes,
    progress: ExchangeProgress,
}

impl PacedBody {
    /// A body of `bytes` that reports to `progress`. Starts the send clock now.
    #[must_use]
    pub fn new(bytes: bytes::Bytes, progress: ExchangeProgress) -> Self {
        progress.on_sent();
        Self {
            remaining: bytes,
            progress,
        }
    }
}

impl http_body::Body for PacedBody {
    type Data = bytes::Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        if self.remaining.is_empty() {
            self.progress.on_sent_all();
            return std::task::Poll::Ready(None);
        }
        let take = self.remaining.len().min(PACED_PIECE);
        let piece = self.remaining.split_to(take);
        if self.remaining.is_empty() {
            self.progress.on_sent_all();
        } else {
            self.progress.on_sent();
        }
        std::task::Poll::Ready(Some(Ok(http_body::Frame::data(piece))))
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.remaining.len() as u64)
    }
}

impl From<PacedBody> for reqwest::Body {
    fn from(body: PacedBody) -> Self {
        reqwest::Body::wrap(body)
    }
}

// ─── Happy Eyeballs at dial ──────────────────────────────────────────────────

/// A bounded per-address TCP connect timeout applied at dial. Keeps a dead path
/// from hanging a connect indefinitely on an adverse network.
pub const DIAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Build the `reqwest` client the upload/fetch paths dial with: rustls, a bounded
/// connect timeout, and the HTTP stack's built-in **Happy Eyeballs v2** at dial.
///
/// Racing happens **at dial only** (networking doc, "Connection racing at dial
/// only"). The parallel IPv6/IPv4 TCP race (RFC 8305) is provided for free by the
/// underlying `hyper-util` `HttpConnector` (a 300 ms fallback delay between the
/// preferred and fallback address families), which both `reqwest` (this client)
/// and `tonic` (the sync channel) sit on — so this module does not re-implement address
/// racing. What this constructor *does* add is the bounded connect timeout and the
/// standing guarantee that Capsule never races whole *requests* across paths
/// (which would double server load): there is no per-request fan-out anywhere in
/// the SDK; a request rides exactly one dialed connection.
pub fn dial_client() -> reqwest::Result<reqwest::Client> {
    http_builder().connect_timeout(DIAL_CONNECT_TIMEOUT).build()
}

// ─── The one HTTP client ─────────────────────────────────────────────────────

/// The request half of the protocol handshake, as default headers for a `reqwest` client.
///
/// Every route the server gates requires `X-Capsule-Protocol` and refuses without it
/// (`capsule-server/src/negotiation.rs`, issue #404), and `X-Capsule-Crypto-Suite` names the
/// suite this build seals under. Both are constants of the build, so they belong on the
/// transport once rather than on every call: a `reqwest` default header rides every request
/// the client sends, the spargen-generated operations included. A header set explicitly on a
/// request still wins, which is how the hand-written upload path keeps pinning a per-transport
/// protocol date.
///
/// `X-Capsule-Sidecar-Schema` rides along too (#404, #520). The design has clients send it on
/// metadata updates, and this build writes exactly one sidecar schema
/// ([`SIDECAR_SCHEMA_V1`](capsule_core::sidecar::SIDECAR_SCHEMA_V1)), so it is a constant of the
/// build like the suite rather than a property of one write. On the transport, every request
/// that carries a sidecar carries the number — including a metadata update reached through a
/// generated operation no hand-written path wraps — and the server validates it only when
/// present, so a request that carries no sidecar is unaffected by it.
#[must_use]
pub fn protocol_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    let mut headers = HeaderMap::with_capacity(3);
    headers.insert(
        HeaderName::from_static(negotiation::PROTOCOL_HEADER),
        HeaderValue::from_static(negotiation::PROTOCOL_VERSION),
    );
    headers.insert(
        HeaderName::from_static("x-capsule-crypto-suite"),
        HeaderValue::from(capsule_core::crypto::primitives::CRYPTO_SUITE_ID),
    );
    headers.insert(
        HeaderName::from_static("x-capsule-sidecar-schema"),
        HeaderValue::from(capsule_core::sidecar::SIDECAR_SCHEMA_V1),
    );
    headers
}

/// The builder every SDK transport starts from: rustls only (the SDK's `reqwest` has no
/// default features and only `rustls-tls`) and the protocol handshake as default headers.
///
/// One builder rather than one client because [`dial_client`] adds a connect timeout on top
/// and the auth, sync and typed clients do not; what they share is the handshake, and this is
/// the one place it is installed. A transport built any other way sends no handshake and is
/// refused by every gated route, which is why nothing in this crate calls
/// `reqwest::Client::builder()` directly outside tests.
pub fn http_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().default_headers(protocol_headers())
}

/// The plain SDK client: [`http_builder`], built.
///
/// # Errors
///
/// Whatever `reqwest` refuses to build with — in practice nothing, since the builder carries
/// no configuration a platform can lack.
pub fn http_client() -> reqwest::Result<reqwest::Client> {
    http_builder().build()
}

#[cfg(test)]
mod tests;
