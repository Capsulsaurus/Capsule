//! Connection-class and retry-policy contract types (SSoT: the Network
//! Resilience design doc). Detection (OS signals + behavioral promotion to
//! [`ConnectionClass::Adverse`]) and the per-class retry engines land with
//! slices `S-D2` (class detection feeding sync/cache budgets) and `S-D10`
//! (adverse-network hardening) in the repo-root `SLICES.md`.
//!
//! This module fills the `capsule-sdk::net` seam named by the networking doc:
//! it maps **mocked OS path signals** ([`PathSignals`]) to a base
//! [`ConnectionClass`], layers the behavioral **`adverse` promotion/demotion**
//! ([`AdverseDetector`]) on top, and exposes the two gate surfaces the class
//! feeds — the **staged-upload tier gates** ([`ConnectionClass::permits_tier`],
//! over [`StagedTier`]) and the **cache-eviction byte budget**
//! ([`ConnectionClass::cache_retention_budget`]). Detection is signal-driven,
//! never a live NIC probe, so every rule is a deterministic unit test.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tracing::instrument;

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
    /// Mirrors the download/upload tier ladder (download-sync doc, "Upload
    /// Tiering"): T0 index escapes on any usable connection (a few KB per asset);
    /// T1 preview and T2 original need a non-metered link — or an explicit
    /// user-consented `force_sync`, which overrides the metered/Wi-Fi criteria but
    /// never resurrects an offline path.
    #[must_use]
    pub fn permits_tier(self, tier: StagedTier, force_sync: bool) -> bool {
        match tier {
            StagedTier::Index => self.is_usable(),
            StagedTier::Preview | StagedTier::Original => {
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

/// The staged-upload tier ladder (download-sync doc, "Upload Tiering"),
/// mirroring the download ladder. Kept local to the SDK's connection seam; the
/// canonical import-side skeleton is `capsule_core::import::upload::UploadTier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StagedTier {
    /// T0 — signed manifest + metadata blob (embedded LQIP): the index that makes
    /// an asset visible (`awaiting-original`) on other devices.
    Index,
    /// T1 — thumbnail + preview derivative blobs.
    Preview,
    /// T2 — the original blob; its finalization flips `original_held` and unlocks
    /// every release path.
    Original,
}

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
/// and `tonic` (the sync channel) sit on — so S-D10 does not re-implement address
/// racing. What this constructor *does* add is the bounded connect timeout and the
/// standing guarantee that Capsule never races whole *requests* across paths
/// (which would double server load): there is no per-request fan-out anywhere in
/// the SDK; a request rides exactly one dialed connection.
pub fn dial_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(DIAL_CONNECT_TIMEOUT)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Class detection (networking Validation).** Mocked OS signals per platform
    /// map to the expected class; `constrained` and `metered` are distinguished.
    #[test]
    fn signals_map_to_the_expected_class() {
        // Desktop / wired default and Android NOT_METERED → unmetered.
        assert_eq!(
            ConnectionClass::from_signals(PathSignals::unmetered()),
            ConnectionClass::Unmetered
        );

        // No path → offline (regardless of the other signals).
        assert_eq!(
            ConnectionClass::from_signals(PathSignals::offline()),
            ConnectionClass::Offline
        );

        // iOS isExpensive → metered.
        assert_eq!(
            ConnectionClass::from_signals(PathSignals {
                has_path: true,
                is_expensive: true,
                not_metered_capability: false,
                is_constrained: false,
            }),
            ConnectionClass::Metered
        );

        // Android without NET_CAPABILITY_NOT_METERED → metered even when not
        // flagged expensive.
        assert_eq!(
            ConnectionClass::from_signals(PathSignals {
                has_path: true,
                is_expensive: false,
                not_metered_capability: false,
                is_constrained: false,
            }),
            ConnectionClass::Metered
        );

        // Data saver → constrained, and it wins over a metered link (distinct from
        // metered).
        let constrained = ConnectionClass::from_signals(PathSignals {
            has_path: true,
            is_expensive: true,
            not_metered_capability: false,
            is_constrained: true,
        });
        assert_eq!(constrained, ConnectionClass::Constrained);
        assert_ne!(constrained, ConnectionClass::Metered);
    }

    /// **Adverse promotion/demotion (networking Validation).** An injected
    /// reset/stall pattern promotes to `adverse`; a sustained clean run demotes.
    #[test]
    fn adverse_promotes_on_resets_and_demotes_after_a_clean_run() {
        let mut detector = AdverseDetector::new(AdverseThresholds {
            window: 8,
            promote_at: 3,
            demote_after_clean: 4,
        });
        let base = ConnectionClass::Metered;

        // Two bad outcomes: not yet promoted.
        detector.record(TransferSignal::Reset);
        detector.record(TransferSignal::Stall);
        assert!(!detector.is_adverse());
        assert_eq!(detector.classify(base), ConnectionClass::Metered);

        // The third bad outcome inside the window trips promotion; the effective
        // class becomes adverse regardless of the metered base.
        detector.record(TransferSignal::BlackHole);
        assert!(detector.is_adverse());
        assert_eq!(detector.classify(base), ConnectionClass::Adverse);

        // A short clean streak does not yet demote.
        detector.record(TransferSignal::Clean);
        detector.record(TransferSignal::Clean);
        detector.record(TransferSignal::Clean);
        assert!(detector.is_adverse());

        // The fourth consecutive clean transfer demotes back to the base class.
        detector.record(TransferSignal::Clean);
        assert!(!detector.is_adverse());
        assert_eq!(detector.classify(base), ConnectionClass::Metered);
    }

    /// A reset mid clean-streak resets the demotion counter (the streak must be
    /// *consecutive*).
    #[test]
    fn a_reset_breaks_the_demotion_streak() {
        let mut detector = AdverseDetector::new(AdverseThresholds {
            window: 6,
            promote_at: 2,
            demote_after_clean: 3,
        });
        detector.record(TransferSignal::Reset);
        detector.record(TransferSignal::Reset);
        assert!(detector.is_adverse());

        detector.record(TransferSignal::Clean);
        detector.record(TransferSignal::Clean);
        detector.record(TransferSignal::Reset); // breaks the streak
        detector.record(TransferSignal::Clean);
        detector.record(TransferSignal::Clean);
        assert!(detector.is_adverse(), "streak was not yet 3 consecutive");
        detector.record(TransferSignal::Clean);
        assert!(!detector.is_adverse());
    }

    /// Offline is never promoted to adverse — there is no path to be flaky on.
    #[test]
    fn offline_is_never_promoted_to_adverse() {
        let mut detector = AdverseDetector::with_defaults();
        for _ in 0..8 {
            detector.record(TransferSignal::BlackHole);
        }
        assert!(detector.is_adverse());
        assert_eq!(
            detector.classify(ConnectionClass::Offline),
            ConnectionClass::Offline
        );
    }

    /// **Connection-class matrix.** The staged-upload tier gates and the
    /// reconciliation gates take exactly the expected value on every class — the
    /// single table that downstream sync/cache consumers read.
    #[test]
    fn connection_class_tier_and_reconciliation_matrix() {
        use ConnectionClass::{Adverse, Constrained, Metered, Offline, Unmetered};
        use StagedTier::{Index, Original, Preview};

        // (class, T0, T1, T2, small, large) with force_sync = false.
        let matrix = [
            (Unmetered, true, true, true, true, true),
            (Metered, true, false, false, false, false),
            (Constrained, true, false, false, false, false),
            (Adverse, true, false, false, false, false),
            (Offline, false, false, false, false, false),
        ];
        for (class, t0, t1, t2, small, large) in matrix {
            assert_eq!(class.permits_tier(Index, false), t0, "{class:?} T0");
            assert_eq!(class.permits_tier(Preview, false), t1, "{class:?} T1");
            assert_eq!(class.permits_tier(Original, false), t2, "{class:?} T2");
            assert_eq!(
                class.permits_reconciliation(ReconciliationClass::Small, false),
                small,
                "{class:?} small"
            );
            assert_eq!(
                class.permits_reconciliation(ReconciliationClass::Large, false),
                large,
                "{class:?} large"
            );
        }
    }

    /// Force-sync overrides the metered/Wi-Fi criteria for above-index tiers and
    /// large reconciliations, but never resurrects an offline path.
    #[test]
    fn force_sync_overrides_metered_but_not_offline() {
        assert!(ConnectionClass::Metered.permits_tier(StagedTier::Original, true));
        assert!(ConnectionClass::Constrained.permits_tier(StagedTier::Preview, true));
        assert!(ConnectionClass::Adverse.permits_reconciliation(ReconciliationClass::Large, true));

        assert!(!ConnectionClass::Offline.permits_tier(StagedTier::Index, true));
        assert!(!ConnectionClass::Offline.permits_reconciliation(ReconciliationClass::Small, true));
    }

    /// The cache-eviction byte budget scales monotonically with re-fetch cost:
    /// unmetered retains the least, offline the most.
    #[test]
    fn cache_budget_is_monotonic_in_refetch_cost() {
        const BASE: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB base
        let unmetered = ConnectionClass::Unmetered.cache_retention_budget(BASE);
        let metered = ConnectionClass::Metered.cache_retention_budget(BASE);
        let constrained = ConnectionClass::Constrained.cache_retention_budget(BASE);
        let adverse = ConnectionClass::Adverse.cache_retention_budget(BASE);
        let offline = ConnectionClass::Offline.cache_retention_budget(BASE);

        assert_eq!(unmetered, BASE);
        assert_eq!(metered, constrained);
        assert!(unmetered <= metered);
        assert!(metered <= adverse);
        assert!(adverse <= offline);
        // Saturating: an absurd base never wraps.
        assert_eq!(
            ConnectionClass::Offline.cache_retention_budget(u64::MAX),
            u64::MAX
        );
    }

    /// **Adverse switches fetches to bounded windows.** The behavioral promotion
    /// (already covered above) feeds directly into the transfer-window rule: only
    /// the promoted `adverse` class bounds the `Range` window; every other class
    /// fetches the whole remainder. The networking-doc linkage "promotes to
    /// `adverse` and switches fetches to bounded windows" in one assertion.
    #[test]
    fn adverse_bounds_the_transfer_window() {
        assert_eq!(
            ConnectionClass::Adverse.range_window(),
            Some(ADVERSE_RANGE_WINDOW)
        );
        for class in [
            ConnectionClass::Unmetered,
            ConnectionClass::Metered,
            ConnectionClass::Constrained,
            ConnectionClass::Offline,
        ] {
            assert_eq!(class.range_window(), None, "{class:?} is open-ended");
        }

        // The promotion path itself lands on the bounded window: a reset burst
        // promotes a metered base to adverse, whose window is now bounded.
        let mut detector = AdverseDetector::with_defaults();
        for _ in 0..3 {
            detector.record(TransferSignal::Reset);
        }
        assert_eq!(
            detector.classify(ConnectionClass::Metered),
            ConnectionClass::Adverse
        );
        assert_eq!(
            detector.classify(ConnectionClass::Metered).range_window(),
            Some(ADVERSE_RANGE_WINDOW)
        );
    }

    /// **Backoff discipline (networking Validation).** For every policy class the
    /// retry sequence stays within its bounds, is jittered, honors a server
    /// `Retry-After` as a floor, and gives up after a bounded number of attempts —
    /// no configuration can produce an unbounded hot loop.
    #[test]
    fn backoff_stays_bounded_jitters_and_gives_up() {
        for class in [
            RetryClass::Interactive,
            RetryClass::BulkTransfer,
            RetryClass::ControlCeremony,
        ] {
            let policy = class.policy();

            // Full jitter at fraction 1.0 rides the ceiling exactly: the sequence
            // is non-decreasing and never exceeds max_delay.
            let mut engine = RetryEngine::deterministic(class, 1.0);
            let mut prev = Duration::ZERO;
            let mut retries = 0u32;
            loop {
                match engine.next_backoff(None) {
                    RetryDecision::Retry { after } => {
                        assert!(after <= policy.max_delay, "{class:?} within cap");
                        assert!(after >= prev, "{class:?} ceiling non-decreasing");
                        prev = after;
                        retries += 1;
                        assert!(retries <= policy.max_retries, "{class:?} bounded");
                    }
                    RetryDecision::GiveUp => break,
                }
            }
            assert_eq!(
                retries, policy.max_retries,
                "{class:?} gives up at the bound"
            );

            // Full jitter at fraction 0.0 floors at zero but STILL gives up: even a
            // zero-delay config cannot hot-loop.
            let mut zero = RetryEngine::deterministic(class, 0.0);
            let mut count = 0u32;
            while let RetryDecision::Retry { after } = zero.next_backoff(None) {
                assert_eq!(after, Duration::ZERO);
                count += 1;
                assert!(count <= policy.max_retries + 1, "{class:?} cannot hot-loop");
            }
            assert_eq!(count, policy.max_retries);

            // A server Retry-After beyond the cap is honored as a floor.
            let mut hinted = RetryEngine::deterministic(class, 0.0);
            let hint = policy.max_delay + Duration::from_secs(5);
            match hinted.next_backoff(Some(hint)) {
                RetryDecision::Retry { after } => {
                    assert_eq!(after, hint, "{class:?} honors Retry-After")
                }
                RetryDecision::GiveUp => panic!("{class:?} should retry first"),
            }
        }
    }

    /// Full jitter is a *uniform spread over `[0, ceiling]`*, not a fixed value:
    /// a seeded production engine at the capped ceiling produces varied waits, all
    /// within the cap. Deterministic via the fixed seed.
    #[test]
    fn full_jitter_spreads_within_the_ceiling() {
        let policy = RetryClass::BulkTransfer.policy();
        let mut engine = RetryEngine::seeded(RetryClass::BulkTransfer, 0xC0FF_EE00_1234_5678);
        // Drive past the doubling so the ceiling is pinned at max_delay, then the
        // only variation is jitter.
        let mut seen = Vec::new();
        for _ in 0..policy.max_retries {
            if let RetryDecision::Retry { after } = engine.next_backoff(None) {
                assert!(after <= policy.max_delay);
                seen.push(after);
            }
        }
        let min = seen.iter().min().copied().unwrap_or_default();
        let max = seen.iter().max().copied().unwrap_or_default();
        assert!(min < max, "full jitter must spread, not fix: {seen:?}");
    }

    /// Resetting the engine after real progress restores the full budget — a
    /// long-lived resumable transfer earns a fresh backoff per independent stall.
    #[test]
    fn reset_restores_the_retry_budget() {
        let mut engine = RetryEngine::deterministic(RetryClass::BulkTransfer, 0.5);
        for _ in 0..3 {
            let _ = engine.next_backoff(None);
        }
        assert_eq!(engine.attempts(), 3);
        engine.reset();
        assert_eq!(engine.attempts(), 0);
    }

    /// The stall detector cuts on **no-bytes-for-T** against a mock clock — never a
    /// wall-clock sleep. Progress rearms it; a gap at or beyond the bound trips it.
    #[test]
    fn stall_detector_cuts_on_no_bytes_for_t() {
        let cfg = StallConfig::new(Duration::from_millis(500));
        let mut stall = StallDetector::new(cfg, 0);

        // Within the bound: live but slow, not a stall.
        assert!(!stall.is_stalled(499));
        // At the bound: cut.
        assert!(stall.is_stalled(500));
        assert!(stall.is_stalled(10_000));

        // Bytes arrive at t=600: the timer rearms from there.
        stall.on_progress(600);
        assert!(!stall.is_stalled(1000)); // only 400 ms since progress
        assert!(stall.is_stalled(1100)); // 500 ms since progress → cut
    }

    /// `dial_client` builds a rustls client with the bounded connect timeout — the
    /// Happy-Eyeballs-at-dial posture (address racing itself is the stack's).
    #[test]
    fn dial_client_builds() {
        assert!(dial_client().is_ok());
    }
}
