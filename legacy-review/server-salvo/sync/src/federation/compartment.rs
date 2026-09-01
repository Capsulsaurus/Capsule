//! Per-peer compartmentalization — each peer is its own blast-radius boundary (slice `S-E2`;
//! SSoT: [Federation — Per-Peer Compartmentalization](https://docs/design/federation/#per-peer-compartmentalization)).
//!
//! Three containment mechanisms, all keyed per peer so a bad peer cannot starve good ones:
//!
//! - **Quotas.** Per-peer budgets on events/hour, bytes/hour, and CPU/hour over a tumbling
//!   1-hour window. Exceeding a budget refuses the request (`429`).
//! - **Error budget + circuit breaker.** Malformed input spends a per-peer error budget; enough
//!   failures trip a breaker that backs the peer off exponentially (5 / 30 / 60 minutes).
//! - **Quarantine for new peers.** First contact starts in a probationary tier with tighter
//!   quotas; a peer graduates to the established tier after a clean period.
//!
//! State is in-memory and clock-injected (every method takes `now`), so the whole state machine is
//! deterministically testable without wall-clock waits.

use std::collections::HashMap;
use std::sync::Mutex;

use jiff::{SignedDuration, Timestamp};

/// The tunable per-peer limits. Deployment-tuned; the defaults are conservative.
#[derive(Debug, Clone)]
pub struct PeerLimits {
    /// Established-tier events/hour.
    pub events_per_hour: u64,
    /// Established-tier bytes/hour.
    pub bytes_per_hour: u64,
    /// Established-tier CPU-milliseconds/hour.
    pub cpu_ms_per_hour: u64,
    /// Probationary-tier events/hour (tighter).
    pub probation_events_per_hour: u64,
    /// Probationary-tier bytes/hour (tighter).
    pub probation_bytes_per_hour: u64,
    /// Probationary-tier CPU-milliseconds/hour (tighter).
    pub probation_cpu_ms_per_hour: u64,
    /// Malformed inputs tolerated before the breaker trips.
    pub error_budget: u32,
    /// Clean-behavior period a probationary peer must clear to graduate.
    pub probation_period: SignedDuration,
    /// Exponential breaker back-offs, indexed by trip count (last entry repeats).
    pub breaker_backoffs: Vec<SignedDuration>,
}

impl Default for PeerLimits {
    fn default() -> Self {
        Self {
            events_per_hour: 10_000,
            bytes_per_hour: 8 * 1024 * 1024 * 1024, // 8 GiB
            cpu_ms_per_hour: 600_000,               // 10 CPU-minutes
            probation_events_per_hour: 1_000,
            probation_bytes_per_hour: 512 * 1024 * 1024, // 512 MiB
            probation_cpu_ms_per_hour: 60_000,           // 1 CPU-minute
            error_budget: 5,
            probation_period: SignedDuration::from_hours(24 * 7), // one week
            breaker_backoffs: vec![
                SignedDuration::from_mins(5),
                SignedDuration::from_mins(30),
                SignedDuration::from_mins(60),
            ],
        }
    }
}

/// A peer's reputation tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTier {
    /// First-contact / recently-seen peer under tighter quotas.
    Probation,
    /// A peer that has behaved cleanly past the probation period.
    Established,
}

/// The measured cost of one pull request, charged against the peer's budgets.
#[derive(Debug, Clone, Copy, Default)]
pub struct PullCost {
    /// Feed events served.
    pub events: u64,
    /// Bytes served.
    pub bytes: u64,
    /// CPU milliseconds spent.
    pub cpu_ms: u64,
}

/// A per-peer containment refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompartmentReject {
    /// The breaker is open; the peer is backed off until this instant.
    CircuitOpen { until: Timestamp },
    /// The events/hour budget would be exceeded.
    EventBudgetExceeded,
    /// The bytes/hour budget would be exceeded.
    ByteBudgetExceeded,
    /// The CPU/hour budget would be exceeded.
    CpuBudgetExceeded,
}

/// One peer's in-memory containment state.
#[derive(Debug, Clone)]
struct PeerState {
    first_seen: Timestamp,
    tier: PeerTier,
    window_start: Timestamp,
    events: u64,
    bytes: u64,
    cpu_ms: u64,
    errors: u32,
    breaker_trips: u32,
    breaker_open_until: Option<Timestamp>,
}

impl PeerState {
    fn new(now: Timestamp, tier: PeerTier) -> Self {
        Self {
            first_seen: now,
            tier,
            window_start: now,
            events: 0,
            bytes: 0,
            cpu_ms: 0,
            errors: 0,
            breaker_trips: 0,
            breaker_open_until: None,
        }
    }

    /// Roll the tumbling budget window if an hour has elapsed.
    fn roll_window(&mut self, now: Timestamp) {
        if now.as_second() - self.window_start.as_second() >= 3_600 {
            self.window_start = now;
            self.events = 0;
            self.bytes = 0;
            self.cpu_ms = 0;
        }
    }

    /// The (events, bytes, cpu_ms) caps for this peer's current tier.
    fn caps(&self, limits: &PeerLimits) -> (u64, u64, u64) {
        match self.tier {
            PeerTier::Probation => (
                limits.probation_events_per_hour,
                limits.probation_bytes_per_hour,
                limits.probation_cpu_ms_per_hour,
            ),
            PeerTier::Established => (
                limits.events_per_hour,
                limits.bytes_per_hour,
                limits.cpu_ms_per_hour,
            ),
        }
    }

    /// Graduate a probationary peer that has cleared the clean-behavior period with a closed
    /// breaker.
    fn maybe_graduate(&mut self, now: Timestamp, limits: &PeerLimits, peer: &str) {
        if self.tier == PeerTier::Probation
            && self.breaker_open_until.is_none()
            && now.as_second() - self.first_seen.as_second() >= limits.probation_period.as_secs()
        {
            self.tier = PeerTier::Established;
            tracing::info!(
                peer,
                "federated peer graduated from probation to established tier"
            );
        }
    }
}

/// The per-peer compartment registry: containment state for every peer this server has served.
pub struct PeerRegistry {
    peers: Mutex<HashMap<String, PeerState>>,
    limits: PeerLimits,
}

impl PeerRegistry {
    /// Build a registry with the given limits.
    #[must_use]
    pub fn new(limits: PeerLimits) -> Self {
        Self {
            peers: Mutex::new(HashMap::new()),
            limits,
        }
    }

    /// Build a registry, seeding known peers into the established tier and treating unknown ones
    /// as first-contact (probation). This is where compartment tiering reconciles with S-C8's
    /// `federation_peers` registration: a peer with a registered identity is not a stranger.
    #[must_use]
    pub fn with_limits(limits: PeerLimits) -> Self {
        Self::new(limits)
    }

    /// Try to admit and charge one pull request of `cost` for `peer` as of `now`. A first-contact
    /// peer defaults to the probation tier. Returns `Ok` (and records the spend) when the breaker
    /// is closed and every budget admits the cost; otherwise refuses without recording the spend.
    ///
    /// `registered` seeds a brand-new peer's tier — a peer already known to `federation_peers` is
    /// not treated as a stranger (established), an unknown one starts in probation.
    #[tracing::instrument(skip(self), fields(peer = %peer))]
    pub fn try_consume(
        &self,
        peer: &str,
        registered: bool,
        cost: PullCost,
        now: Timestamp,
    ) -> Result<PeerTier, CompartmentReject> {
        let mut guard = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = guard.entry(peer.to_string()).or_insert_with(|| {
            let tier = if registered {
                PeerTier::Established
            } else {
                PeerTier::Probation
            };
            PeerState::new(now, tier)
        });

        // Breaker: refuse while open; on elapse, half-open (reset the error budget).
        if let Some(until) = state.breaker_open_until {
            if now < until {
                return Err(CompartmentReject::CircuitOpen { until });
            }
            state.breaker_open_until = None;
            state.errors = 0;
        }

        state.maybe_graduate(now, &self.limits, peer);
        state.roll_window(now);

        let (evt_cap, byte_cap, cpu_cap) = state.caps(&self.limits);
        if state.events + cost.events > evt_cap {
            tracing::warn!(cap = evt_cap, "peer events/hour budget exceeded");
            return Err(CompartmentReject::EventBudgetExceeded);
        }
        if state.bytes + cost.bytes > byte_cap {
            tracing::warn!(cap = byte_cap, "peer bytes/hour budget exceeded");
            return Err(CompartmentReject::ByteBudgetExceeded);
        }
        if state.cpu_ms + cost.cpu_ms > cpu_cap {
            tracing::warn!(cap = cpu_cap, "peer CPU/hour budget exceeded");
            return Err(CompartmentReject::CpuBudgetExceeded);
        }

        state.events += cost.events;
        state.bytes += cost.bytes;
        state.cpu_ms += cost.cpu_ms;
        Ok(state.tier)
    }

    /// Record a malformed-input error against `peer`. When the error budget is spent the breaker
    /// trips, backing the peer off for the next (exponentially larger) interval. Returns the
    /// instant the breaker is open until, if it tripped.
    #[tracing::instrument(skip(self), fields(peer = %peer))]
    pub fn record_error(&self, peer: &str, now: Timestamp) -> Option<Timestamp> {
        let mut guard = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = guard
            .entry(peer.to_string())
            .or_insert_with(|| PeerState::new(now, PeerTier::Probation));
        state.errors += 1;
        if state.errors > self.limits.error_budget {
            let idx = (state.breaker_trips as usize).min(self.limits.breaker_backoffs.len() - 1);
            let backoff = self.limits.breaker_backoffs[idx];
            let until = now.checked_add(backoff).unwrap_or(now);
            state.breaker_open_until = Some(until);
            state.breaker_trips += 1;
            state.errors = 0;
            tracing::warn!(until = %until, trips = state.breaker_trips, "circuit breaker tripped for peer");
            return Some(until);
        }
        None
    }

    /// The peer's current tier, if it has been seen. Test/observability inspector.
    #[must_use]
    pub fn tier(&self, peer: &str) -> Option<PeerTier> {
        let guard = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(peer).map(|s| s.tier)
    }

    /// Whether `peer`'s breaker is currently open as of `now`.
    #[must_use]
    pub fn is_circuit_open(&self, peer: &str, now: Timestamp) -> bool {
        let guard = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .get(peer)
            .and_then(|s| s.breaker_open_until)
            .is_some_and(|until| now < until)
    }
}
