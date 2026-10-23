use capsule_core::notify::{self, AlertClass, NotifyInput};

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

// ── Projection ──────────────────────────────────────────────────

/// The projection walks every [`VerificationState`], and the fact it hands the shared
/// predicate agrees with the state the UX renders at the same instant.
#[test]
fn notify_facts_projects_every_state() {
    let due = BASE + INITIAL_INTERVAL_SECS;

    // Verified → the prompt is scheduled, nothing is snoozed, budget intact.
    let cad = RecoveryCadence::armed_at_setup(ts(BASE));
    let facts = cad.notify_facts(ts(BASE));
    assert_eq!(facts.next_due, ts(due));
    assert_eq!(facts.snoozed_until, None);
    assert!(!facts.snooze_budget_spent);

    // Due → the same `next_due`, now in the past, so the predicate fires.
    let facts = cad.notify_facts(ts(due));
    assert_eq!(facts.next_due, ts(due));
    assert_eq!(facts.snoozed_until, None);
    assert!(!facts.snooze_budget_spent);

    // Snoozed within budget → the snooze end is carried, the budget is not spent.
    let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
    cad.snooze(ts(due), SnoozeDuration::OneDay);
    let facts = cad.notify_facts(ts(due));
    assert_eq!(facts.snoozed_until, Some(ts(due + DAY_SECS)));
    assert!(!facts.snooze_budget_spent);

    // The budget spends on the third consecutive snooze, and the last one still holds.
    let mut at = due;
    for _ in 1..MAX_CONSECUTIVE_SNOOZES {
        at += DAY_SECS;
        cad.snooze(ts(at), SnoozeDuration::OneDay);
    }
    let facts = cad.notify_facts(ts(at));
    assert_eq!(facts.snoozed_until, Some(ts(at + DAY_SECS)));
    assert!(facts.snooze_budget_spent);

    // Badge → the snooze has expired with the budget spent: reported, never pre-armed.
    let after = at + DAY_SECS;
    assert_eq!(cad.state(ts(after)), VerificationState::Badge);
    let facts = cad.notify_facts(ts(after));
    assert_eq!(facts.next_due, ts(due));
    assert_eq!(facts.snoozed_until, None);
    assert!(facts.snooze_budget_spent);

    // None of the scheduled states is a re-wrap escalation.
    for at in [BASE, due, at, after] {
        assert!(!cad.notify_facts(ts(at)).rewrap_due, "at {at}");
    }
}

/// `RewrapDue` is due now, whatever the ladder says: an explicit "I lost it" is not a
/// scheduled check, and the closed class set has only `recovery_check_due` to carry it.
#[test]
fn notify_facts_reports_rewrap_due_immediately() {
    let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
    cad.declare_lost();
    assert_eq!(cad.state(ts(BASE)), VerificationState::RewrapDue);

    // The ladder still says "not for 7 days"; the projection says "now".
    assert_eq!(cad.next_due(), ts(BASE + INITIAL_INTERVAL_SECS));
    let facts = cad.notify_facts(ts(BASE));
    assert_eq!(facts.next_due, ts(BASE));
    assert_eq!(facts.snoozed_until, None);
    assert!(!facts.snooze_budget_spent);
    assert!(
        facts.rewrap_due,
        "the escalation must be distinguishable from a routine check"
    );

    // And it reaches the alert as a parameter, not just as an earlier due date.
    let input = NotifyInput {
        recovery: Some(facts),
        ..NotifyInput::default()
    };
    let alert = notify::evaluate(&input, ts(BASE))
        .into_iter()
        .find(|a| a.class == AlertClass::RecoveryCheckDue)
        .expect("due now");
    assert_eq!(alert.params["recovery"], "rewrap");
}

/// The projection is the whole of the wiring: what the scheduler says and what the shared
/// predicate decides never disagree about whether a check is due.
#[test]
fn notify_facts_agrees_with_the_rendered_state() {
    let mut cad = RecoveryCadence::armed_at_setup(ts(BASE));
    cad.snooze(ts(BASE + INITIAL_INTERVAL_SECS), SnoozeDuration::OneWeek);

    for offset in [0, INITIAL_INTERVAL_SECS - 1, INITIAL_INTERVAL_SECS] {
        let now = ts(BASE + offset);
        let input = NotifyInput {
            recovery: Some(cad.notify_facts(now)),
            ..NotifyInput::default()
        };
        let fired = notify::evaluate(&input, now)
            .iter()
            .any(|a| a.class == AlertClass::RecoveryCheckDue);
        let rendered_due = matches!(
            cad.state(now),
            VerificationState::Due | VerificationState::Badge | VerificationState::RewrapDue
        );
        assert_eq!(fired, rendered_due, "at +{offset}s");
    }
}
