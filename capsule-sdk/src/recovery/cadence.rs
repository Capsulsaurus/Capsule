//! The recovery-verification **cadence scheduler** and the **prompt state machine**
//! the client UX renders (SSoT: [Backup — Recovery Verification
//! Cadence]).
//!
//! This half of the recovery verification is deliberately **pure and network-free**: every method
//! takes the current wall-clock instant as an argument, so the whole ladder — the 7 d → 90 d → 180
//! d back-off, the re-arm triggers, the snooze caps, and the repeated-failure escalation to a
//! guided re-wrap — is driven by a mocked clock in tests with no sleeps and no I/O. The scheduler
//! is `serde`-serializable so a client persists it across launches.
//!
//! **Advisory by construction.** The scheduler exposes exactly one read surface —
//! [`RecoveryCadence::state`] — returning a [`VerificationState`] the UX renders. There
//! is no method anywhere that gates sync, unlock, or any critical flow;
//! [`VerificationState::blocks_critical_flow`] is `const false` for every variant, so the
//! "never blocks" rule is a compile-time property, not a convention.
//!
//! [Backup — Recovery Verification Cadence]: https://docs/design/backup-recovery/#recovery-verification-cadence

use capsule_core::notify::RecoveryFacts;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// One day, in seconds — the unit the whole ladder is expressed in.
const DAY_SECS: i64 = 86_400;

/// First prompt lands **7 days** after setup: catches the lost-napkin case while
/// re-setup is still cheap.
pub const INITIAL_INTERVAL_SECS: i64 = 7 * DAY_SECS;
/// After the first success the interval steps to **90 days**.
pub const BACKOFF_INTERVAL_SECS: i64 = 90 * DAY_SECS;
/// After two consecutive successes the interval caps at **180 days** and stays there.
pub const CAP_INTERVAL_SECS: i64 = 180 * DAY_SECS;

/// At most this many consecutive snoozes before the prompt degrades to a persistent,
/// non-blocking [`VerificationState::Badge`].
pub const MAX_CONSECUTIVE_SNOOZES: u8 = 3;

/// Failures required before the guided re-wrap is offered — **and** they must span at
/// least [`REWRAP_MIN_SESSIONS`] distinct app sessions, so a single fat-fingered
/// session never triggers a rotation.
pub const REWRAP_FAILURE_THRESHOLD: u32 = 3;
/// The re-wrap escalation requires failures across at least this many app sessions.
pub const REWRAP_MIN_SESSIONS: u32 = 2;

/// How long a user may defer a due prompt. The check is advisory, so a snooze only
/// moves the next reminder — it never suppresses anything critical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnoozeDuration {
    /// Remind again in 24 hours.
    OneDay,
    /// Remind again in 7 days.
    OneWeek,
}

impl SnoozeDuration {
    /// The snooze length in seconds.
    #[must_use]
    pub const fn secs(self) -> i64 {
        match self {
            Self::OneDay => DAY_SECS,
            Self::OneWeek => 7 * DAY_SECS,
        }
    }
}

/// A reason to reset the ladder back to the 7-day step (SSoT § Schedule and Triggers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RearmTrigger {
    /// A new device enrolled — the prompt lands on the *new* device, which has never
    /// seen the passphrase.
    DeviceEnrolled,
    /// The recovery secret rotated: an escrow store that
    /// [replaced](crate::recovery::EscrowStored::replaced) an earlier one, e.g. a guided
    /// re-wrap. A store that replaced nothing is a first enrollment, which is
    /// [`RecoveryCadence::armed_at_setup`], not a re-arm.
    SecretRotated,
    /// A restore-from-escrow completed on this device.
    RestoredFromEscrow,
}

/// The state the client UX renders. This is the entire consumer contract of the
/// scheduler — pure data, never a localized string, and never a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationState {
    /// The secret was last verified and the next prompt is scheduled for `next_due`.
    /// Nothing to show now.
    Verified {
        /// When the next verification prompt becomes due.
        next_due: Timestamp,
    },
    /// A verification prompt is due now.
    Due,
    /// The user snoozed; the prompt is suppressed until `until`. `snoozes_used` of the
    /// [`MAX_CONSECUTIVE_SNOOZES`] budget have been consumed.
    Snoozed {
        /// When the snooze expires and the prompt becomes due again.
        until: Timestamp,
        /// Consecutive snoozes consumed so far.
        snoozes_used: u8,
    },
    /// The snooze budget is exhausted: a persistent, **non-blocking** badge remains
    /// until the user verifies. Not a lock — advisory only.
    Badge,
    /// Repeated failures (≥ [`REWRAP_FAILURE_THRESHOLD`] across ≥ [`REWRAP_MIN_SESSIONS`]
    /// app sessions) or an explicit "I lost it": the client should run the guided
    /// re-wrap flow ([`crate::recovery::RecoveryClient::guided_rewrap`]).
    RewrapDue,
}

impl VerificationState {
    /// The verification cadence is advisory by design. **No** state — not even
    /// [`Badge`](Self::Badge) or [`RewrapDue`](Self::RewrapDue) — ever blocks sync,
    /// unlock, or any critical flow. This is `const false` for every variant so the
    /// "never blocks" invariant is enforced by the type, not by discipline.
    #[must_use]
    pub const fn blocks_critical_flow(self) -> bool {
        false
    }
}

/// The persistent recovery-verification scheduler for one account on one device.
///
/// Construct it at setup with [`RecoveryCadence::armed_at_setup`]; drive it with
/// [`record_success`](Self::record_success), [`record_failure`](Self::record_failure),
/// [`snooze`](Self::snooze), [`rearm`](Self::rearm), and [`declare_lost`](Self::declare_lost);
/// render [`state`](Self::state) at `now`. All time arguments are explicit, so the whole
/// machine is deterministic under a mocked clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCadence {
    /// When the next verification prompt is due.
    next_due: Timestamp,
    /// Consecutive successful verifications (drives the ladder; capped in effect at 2).
    consecutive_successes: u32,
    /// Consecutive snoozes since the last verification.
    snoozes_used: u8,
    /// When the active snooze expires, if any.
    snoozed_until: Option<Timestamp>,
    /// Total failures since the last success.
    failures: u32,
    /// Distinct app sessions in which a failure was recorded since the last success.
    distinct_failure_sessions: u32,
    /// The app-session id of the most recent failure, to count *distinct* sessions.
    last_failure_session: Option<u64>,
    /// The user explicitly declared the secret lost — jump straight to re-wrap.
    declared_lost: bool,
}

impl RecoveryCadence {
    /// Arm the ladder at account setup: the first prompt lands 7 days out.
    #[must_use]
    pub fn armed_at_setup(now: Timestamp) -> Self {
        Self {
            next_due: add_secs(now, INITIAL_INTERVAL_SECS),
            consecutive_successes: 0,
            snoozes_used: 0,
            snoozed_until: None,
            failures: 0,
            distinct_failure_sessions: 0,
            last_failure_session: None,
            declared_lost: false,
        }
    }

    /// The interval to the next prompt given the number of consecutive successes so
    /// far: 7 d for the initial check, 90 d after one success, 180 d (capped) after two.
    #[must_use]
    pub const fn interval_for(consecutive_successes: u32) -> i64 {
        match consecutive_successes {
            0 => INITIAL_INTERVAL_SECS,
            1 => BACKOFF_INTERVAL_SECS,
            _ => CAP_INTERVAL_SECS,
        }
    }

    /// When the next prompt is currently scheduled.
    #[must_use]
    pub const fn next_due(&self) -> Timestamp {
        self.next_due
    }

    /// Whether the repeated-failure (or explicit-loss) re-wrap threshold has tripped.
    #[must_use]
    pub const fn rewrap_due(&self) -> bool {
        self.declared_lost
            || (self.failures >= REWRAP_FAILURE_THRESHOLD
                && self.distinct_failure_sessions >= REWRAP_MIN_SESSIONS)
    }

    /// The state the UX should render at `now`.
    #[must_use]
    pub fn state(&self, now: Timestamp) -> VerificationState {
        // Repeated failure / explicit loss dominates: the user needs a new secret.
        if self.rewrap_due() {
            return VerificationState::RewrapDue;
        }
        // An active snooze suppresses the prompt until it expires.
        if let Some(until) = self.snoozed_until
            && now < until
        {
            return VerificationState::Snoozed {
                until,
                snoozes_used: self.snoozes_used,
            };
        }
        // Due yet?
        if now >= self.next_due {
            if self.snoozes_used >= MAX_CONSECUTIVE_SNOOZES {
                // Snooze budget spent → persistent, non-blocking badge.
                return VerificationState::Badge;
            }
            return VerificationState::Due;
        }
        VerificationState::Verified {
            next_due: self.next_due,
        }
    }

    /// Record a successful verification: advance the ladder, and clear the snooze and
    /// failure counters (a success re-arms the "consecutive" accounting).
    pub fn record_success(&mut self, now: Timestamp) {
        self.consecutive_successes = self.consecutive_successes.saturating_add(1);
        self.snoozes_used = 0;
        self.snoozed_until = None;
        self.failures = 0;
        self.distinct_failure_sessions = 0;
        self.last_failure_session = None;
        self.declared_lost = false;
        self.next_due = add_secs(now, Self::interval_for(self.consecutive_successes));
    }

    /// Record a failed verification during app session `session`. A failure keeps the
    /// prompt due (it does not reschedule), but once
    /// [`REWRAP_FAILURE_THRESHOLD`] failures span [`REWRAP_MIN_SESSIONS`] distinct
    /// sessions the state escalates to [`VerificationState::RewrapDue`]. Returns the
    /// state at `now` for the caller's convenience.
    pub fn record_failure(&mut self, now: Timestamp, session: u64) -> VerificationState {
        self.failures = self.failures.saturating_add(1);
        if self.last_failure_session != Some(session) {
            self.distinct_failure_sessions = self.distinct_failure_sessions.saturating_add(1);
            self.last_failure_session = Some(session);
        }
        self.state(now)
    }

    /// Snooze a due prompt. Permitted up to [`MAX_CONSECUTIVE_SNOOZES`] times; the
    /// `N+1`-th request is a no-op that leaves the prompt in the persistent
    /// [`VerificationState::Badge`] state. Returns the resulting state.
    pub fn snooze(&mut self, now: Timestamp, dur: SnoozeDuration) -> VerificationState {
        if self.snoozes_used >= MAX_CONSECUTIVE_SNOOZES {
            // Budget exhausted: no further deferral — the badge stands.
            self.snoozed_until = None;
            return self.state(now);
        }
        self.snoozes_used = self.snoozes_used.saturating_add(1);
        self.snoozed_until = Some(add_secs(now, dur.secs()));
        self.state(now)
    }

    /// Re-arm the ladder back to the 7-day step (new device, secret rotation, or a
    /// completed restore-from-escrow), clearing snoozes and failures.
    pub fn rearm(&mut self, now: Timestamp, _trigger: RearmTrigger) {
        self.consecutive_successes = 0;
        self.snoozes_used = 0;
        self.snoozed_until = None;
        self.failures = 0;
        self.distinct_failure_sessions = 0;
        self.last_failure_session = None;
        self.declared_lost = false;
        self.next_due = add_secs(now, INITIAL_INTERVAL_SECS);
    }

    /// The user explicitly said "I lost it": escalate straight to the guided re-wrap.
    pub fn declare_lost(&mut self) {
        self.declared_lost = true;
    }

    /// Project the scheduler into the flat facts the shared alert predicate consumes
    /// ([`capsule_core::notify`]).
    ///
    /// The direction matters: `capsule-sdk` depends on `capsule-core` and never the reverse, so
    /// the predicate cannot read this scheduler. It is handed a projection instead, and this is
    /// the only place that mapping exists — the ladder, the re-arm triggers, and the snooze
    /// accounting stay owned here, and `capsule_core::notify` never recomputes them.
    ///
    /// It is derived from [`state`](Self::state) rather than from the fields, so the alert and
    /// the prompt the UX renders can never disagree about whether a check is due. Two mappings
    /// are worth stating:
    ///
    /// - [`VerificationState::Badge`] reports `snooze_budget_spent`, which is what stops the
    ///   alert being pre-armed as a notification while leaving the condition reported — the
    ///   badge is persistent and non-blocking, and never escalates back into an alert.
    /// - [`VerificationState::RewrapDue`] is due **now**, whatever the ladder says: repeated
    ///   failure or an explicit "I lost it" is not a scheduled check. The alert class set is
    ///   closed, so `recovery_check_due` is the only class that can carry it — which is why the
    ///   escalation also rides [`RecoveryFacts::rewrap_due`], surfacing as the alert's
    ///   `recovery` parameter. Without it the alert would be byte-identical to the routine
    ///   ninety-day check, and the client could not know to route into
    ///   [`RecoveryClient::guided_rewrap`](crate::recovery::RecoveryClient::guided_rewrap)
    ///   rather than a plain verification prompt.
    #[must_use]
    pub fn notify_facts(&self, now: Timestamp) -> RecoveryFacts {
        match self.state(now) {
            VerificationState::Verified { next_due } => RecoveryFacts {
                next_due,
                snoozed_until: None,
                snooze_budget_spent: false,
                rewrap_due: false,
            },
            VerificationState::Due => RecoveryFacts {
                next_due: self.next_due,
                snoozed_until: None,
                snooze_budget_spent: false,
                rewrap_due: false,
            },
            VerificationState::Snoozed {
                until,
                snoozes_used,
            } => RecoveryFacts {
                next_due: self.next_due,
                snoozed_until: Some(until),
                snooze_budget_spent: snoozes_used >= MAX_CONSECUTIVE_SNOOZES,
                rewrap_due: false,
            },
            VerificationState::Badge => RecoveryFacts {
                next_due: self.next_due,
                snoozed_until: None,
                snooze_budget_spent: true,
                rewrap_due: false,
            },
            VerificationState::RewrapDue => RecoveryFacts {
                next_due: now,
                snoozed_until: None,
                snooze_budget_spent: false,
                rewrap_due: true,
            },
        }
    }
}

/// Add a signed second offset to a timestamp, saturating at the representable bounds
/// (the scheduler never operates near them; saturation just keeps the API total).
fn add_secs(base: Timestamp, secs: i64) -> Timestamp {
    let target = base.as_second().saturating_add(secs);
    Timestamp::from_second(target).unwrap_or(if target < 0 {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    })
}

#[cfg(test)]
mod tests;
