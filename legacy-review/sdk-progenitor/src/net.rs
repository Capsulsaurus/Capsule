//! Connection-class and retry-policy contract types (SSoT: the Network
//! Resilience design doc). Detection (OS signals + behavioral promotion to
//! [`ConnectionClass::Adverse`]) and the per-class retry engines land with
//! slices `S-D2` (class detection feeding sync/cache budgets) and `S-D10`
//! (adverse-network hardening) in the repo-root `SLICES.md`.

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
