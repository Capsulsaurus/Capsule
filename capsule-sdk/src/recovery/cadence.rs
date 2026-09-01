//! The recovery-verification **cadence scheduler** and the **prompt state machine**
//! the client UX renders (slice `S-D12`; SSoT: [Backup — Recovery Verification
//! Cadence]).
//!
//! This half of S-D12 is deliberately **pure and network-free**: every method takes
//! the current wall-clock instant as an argument, so the whole ladder — the 7 d → 90 d
//! → 180 d back-off, the re-arm triggers, the snooze caps, and the repeated-failure
//! escalation to a guided re-wrap — is driven by a mocked clock in tests with no
//! sleeps and no I/O. The scheduler is `serde`-serializable so a client persists it
//! across launches.
//!
//! **Advisory by construction.** The scheduler exposes exactly one read surface —
//! [`RecoveryCadence::state`] — returning a [`VerificationState`] the UX renders. There
//! is no method anywhere that gates sync, unlock, or any critical flow;
//! [`VerificationState::blocks_critical_flow`] is `const false` for every variant, so the
//! "never blocks" rule is a compile-time property, not a convention.
//!
//! [Backup — Recovery Verification Cadence]: https://docs/design/backup-recovery/#recovery-verification-cadence

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
    /// The recovery secret rotated (e.g. after a guided re-wrap).
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
mod tests {
    use super::*;

    /// A fixed, round base instant well away from the timestamp bounds.
    const BASE: i64 = 1_700_000_000;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    /// SSoT Validation: "7 d → 90 d → 180 d back-off." A mocked clock walks the ladder;
    /// each success steps the interval and it caps at 180 d.
    #[test]
    fn ladder_7d_90d_180d_backoff() {
        let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));

        // Setup → first prompt 7 days out.
        assert_eq!(cad.next_due(), ts(BASE + INITIAL_INTERVAL_SECS));
        assert_eq!(
            cad.state(ts(BASE + INITIAL_INTERVAL_SECS - 1)),
            VerificationState::Verified {
                next_due: ts(BASE + INITIAL_INTERVAL_SECS)
            }
        );
        assert_eq!(
            cad.state(ts(BASE + INITIAL_INTERVAL_SECS)),
            VerificationState::Due
        );

        // First success → next prompt 90 days out.
        let t1 = BASE + INITIAL_INTERVAL_SECS;
        cad.record_success(ts(t1));
        assert_eq!(cad.next_due(), ts(t1 + BACKOFF_INTERVAL_SECS));

        // Second consecutive success → caps at 180 days.
        let t2 = t1 + BACKOFF_INTERVAL_SECS;
        cad.record_success(ts(t2));
        assert_eq!(cad.next_due(), ts(t2 + CAP_INTERVAL_SECS));

        // Third success → stays capped at 180 days.
        let t3 = t2 + CAP_INTERVAL_SECS;
        cad.record_success(ts(t3));
        assert_eq!(cad.next_due(), ts(t3 + CAP_INTERVAL_SECS));
    }

    /// SSoT Validation: "re-arm on device-add, rotation, and restore." Each trigger
    /// resets the ladder to the 7-day step from the re-arm moment.
    #[test]
    fn rearm_resets_to_7day_step() {
        for trigger in [
            RearmTrigger::DeviceEnrolled,
            RearmTrigger::SecretRotated,
            RearmTrigger::RestoredFromEscrow,
        ] {
            let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
            // Climb to the 180-day cap first.
            cad.record_success(ts(BASE));
            cad.record_success(ts(BASE));
            assert_eq!(cad.next_due(), ts(BASE + CAP_INTERVAL_SECS));

            // Re-arm at a later moment → back to +7 days from *there*.
            let rearm_at = BASE + 1_000;
            cad.rearm(ts(rearm_at), trigger);
            assert_eq!(cad.next_due(), ts(rearm_at + INITIAL_INTERVAL_SECS));
            // And the ladder starts over: next success is +90 d, not +180 d.
            let due = rearm_at + INITIAL_INTERVAL_SECS;
            cad.record_success(ts(due));
            assert_eq!(cad.next_due(), ts(due + BACKOFF_INTERVAL_SECS));
        }
    }

    /// SSoT Validation: "snooze caps." Three consecutive snoozes are allowed; the
    /// fourth cannot defer further and the prompt degrades to a persistent badge.
    #[test]
    fn snooze_caps_then_badge() {
        let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
        let due = BASE + INITIAL_INTERVAL_SECS;
        assert_eq!(cad.state(ts(due)), VerificationState::Due);

        // Snooze 1, 2, 3 — each defers and increments the counter.
        let mut now = due;
        for used in 1..=MAX_CONSECUTIVE_SNOOZES {
            let st = cad.snooze(ts(now), SnoozeDuration::OneDay);
            assert_eq!(
                st,
                VerificationState::Snoozed {
                    until: ts(now + DAY_SECS),
                    snoozes_used: used,
                }
            );
            // Within the snooze window it stays suppressed…
            assert!(matches!(
                cad.state(ts(now + DAY_SECS - 1)),
                VerificationState::Snoozed { .. }
            ));
            // …and when it expires it is due again (until the budget is spent).
            now += DAY_SECS;
        }

        // Budget spent: the prompt is now a persistent, non-blocking badge.
        assert_eq!(cad.state(ts(now)), VerificationState::Badge);
        // A fourth snooze cannot defer — still a badge.
        assert_eq!(
            cad.snooze(ts(now), SnoozeDuration::OneWeek),
            VerificationState::Badge
        );

        // A success clears the snooze budget entirely.
        cad.record_success(ts(now));
        assert!(matches!(
            cad.state(ts(now)),
            VerificationState::Verified { .. }
        ));
    }

    /// The 7-day snooze variant defers by a week.
    #[test]
    fn snooze_one_week_defers_seven_days() {
        let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
        let due = BASE + INITIAL_INTERVAL_SECS;
        let st = cad.snooze(ts(due), SnoozeDuration::OneWeek);
        assert_eq!(
            st,
            VerificationState::Snoozed {
                until: ts(due + 7 * DAY_SECS),
                snoozes_used: 1,
            }
        );
    }

    /// SSoT § On Repeated Failure: 3 failures across ≥ 2 app sessions escalate to
    /// re-wrap; the same 3 failures within a *single* session do not.
    #[test]
    fn repeated_failure_needs_two_sessions() {
        // Three failures, all in session 1 → not enough sessions → still just Due.
        let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
        let due = BASE + INITIAL_INTERVAL_SECS;
        cad.record_failure(ts(due), 1);
        cad.record_failure(ts(due), 1);
        let st = cad.record_failure(ts(due), 1);
        assert_eq!(st, VerificationState::Due);
        assert!(!cad.rewrap_due());

        // A third failure that lands in a *second* session tips it over.
        let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
        cad.record_failure(ts(due), 1);
        cad.record_failure(ts(due), 1);
        let st = cad.record_failure(ts(due), 2);
        assert_eq!(st, VerificationState::RewrapDue);
        assert!(cad.rewrap_due());
    }

    /// An explicit "I lost it" jumps straight to re-wrap, no failure count needed.
    #[test]
    fn declare_lost_escalates_immediately() {
        let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
        cad.declare_lost();
        assert_eq!(cad.state(ts(BASE)), VerificationState::RewrapDue);
    }

    /// A success after failures clears the failure accounting (no lingering escalation).
    #[test]
    fn success_clears_failure_accounting() {
        let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
        let due = BASE + INITIAL_INTERVAL_SECS;
        cad.record_failure(ts(due), 1);
        cad.record_failure(ts(due), 2);
        cad.record_success(ts(due));
        assert!(!cad.rewrap_due());
        cad.record_failure(ts(due), 3);
        // Only one fresh failure post-success → nowhere near the threshold.
        assert!(!cad.rewrap_due());
    }

    /// SSoT Validation: "never blocks a critical flow." No state blocks — the invariant
    /// is a compile-time `const false`, exercised here across every variant.
    #[test]
    fn no_state_ever_blocks() {
        for st in [
            VerificationState::Verified { next_due: ts(BASE) },
            VerificationState::Due,
            VerificationState::Snoozed {
                until: ts(BASE),
                snoozes_used: 3,
            },
            VerificationState::Badge,
            VerificationState::RewrapDue,
        ] {
            assert!(!st.blocks_critical_flow());
        }
    }

    /// The scheduler round-trips through its `serde` form (clients persist it).
    #[test]
    fn cadence_state_serde_round_trips() {
        let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
        cad.record_success(ts(BASE));
        cad.snooze(ts(BASE + BACKOFF_INTERVAL_SECS), SnoozeDuration::OneDay);
        let json = serde_json::to_string(&cad).unwrap();
        let back: RecoveryCadence = serde_json::from_str(&json).unwrap();
        assert_eq!(cad, back);
    }
}
