//! The trigger predicates: [`evaluate()`] (which classes are true now) and [`next_deadline()`]
//! (the one instant to arm an OS timer for).
//!
//! Each predicate cites the doc that owns its threshold. This module owns none of them — it owns
//! only the *composition*, which is the thing that must not be reimplemented per platform.
//!
//! # Boundary convention
//!
//! Every threshold in this module **fires at the boundary instant**: `now >= deadline`, never
//! `>`. That matches the recovery cadence's own `now >= next_due`, so a client that renders both
//! never sees them disagree by a second. Suppression is the mirror image and is exclusive
//! (`until > now`), so a class snoozed to exactly `now` is due at `now`.

use std::collections::BTreeMap;

use jiff::Timestamp;

use super::class::{Alert, AlertClass};
use super::input::{NotifyInput, QuotaAdvisory, RecoveryFacts, SyncFacts};

/// One day, in seconds — the unit the staleness threshold is expressed in.
pub const DAY_SECS: i64 = 86_400;

/// The staleness threshold: **two weeks** without a completed sync while changes remain
/// un-synced. From [Download & Sync — Notifications], which owns the predicate; this module owns
/// only its evaluation.
///
/// [Download & Sync — Notifications]: https://docs/design/import/download-sync/#notifications
pub const SYNC_STALE_SECS: i64 = 14 * DAY_SECS;

/// Every alert that is true at `now`, in [`AlertClass::ALL`] order.
///
/// Pure: `now` is an argument, nothing is read from the environment, and equal inputs give
/// equal outputs. A suppressed class is skipped before its predicate runs, so suppression can
/// never be observed as "fired but hidden".
///
/// Absent facts emit nothing: `None` sync facts, `None` quota facts and zero counts are all
/// silence, not an error.
#[must_use]
pub fn evaluate(input: &NotifyInput, now: Timestamp) -> Vec<Alert> {
    let mut alerts = Vec::new();
    for class in AlertClass::ALL {
        if input.is_suppressed(class, now) {
            continue;
        }
        if let Some(alert) = evaluate_class(input, class, now) {
            alerts.push(alert);
        }
    }
    // The whole point of one shared decision function is that a field report says which
    // classes a device decided were true, without the device having to explain itself.
    tracing::debug!(
        now = %now,
        classes = ?alerts.iter().map(|a| a.class.as_str()).collect::<Vec<_>>(),
        "notify: evaluated the alert classes"
    );
    alerts
}

/// The instant an OS timer must be armed for, **per class** — the arm half of the
/// arm / re-arm / cancel rule.
///
/// A class present in the map should have exactly one live timer, set to the returned instant.
/// A class *absent* from it should have no timer: cancel whatever it holds. Because the answer
/// is a pure function of state, the whole client-side protocol is "recompute after any state
/// change, then reconcile your timers against this map" — and one entry per class from one
/// function is why two live timers for one class is structurally impossible.
///
/// It is keyed per class rather than collapsed to a single instant because the timers are
/// independent: with a staleness deadline two weeks out and a recovery check ninety days out,
/// arming only the earlier one loses the later alert entirely on a device the app never runs on
/// again — which is the exact case pre-arming exists for. A client also needs the class to pick
/// its `notification.*` catalog key for the notification it is arming.
///
/// This is deliberately **narrower** than [`evaluate()`]. An armed notification fires from the
/// OS's own timer with the app not running, so it cannot be re-checked when it arrives: an
/// instant is returned only when the alert is certain to be true on arrival. Three things
/// therefore withhold one:
///
/// - the class is not [pre-armable](AlertClass::pre_armable) — its condition is server-held, so
///   the device cannot compute a deadline for it at all;
/// - the resulting instant is not strictly after `now` (it has already passed, so there is
///   nothing left to schedule) — including a class the alert has already fired for;
/// - the class would arrive as something other than a notification: `sync_stale` with nothing
///   un-synced (only a sync can change that, and a sync re-arms), `recovery_check_due` with its
///   snooze budget spent (a badge, which is in-app), and any class the user has
///   [disabled](NotifyInput::disabled).
///
/// A [snooze](NotifyInput::suppressed) **defers** the armed instant rather than cancelling it: a
/// class snoozed after it fired must re-fire when the snooze ends, and the snooze end is a
/// deadline the device can compute.
#[must_use]
pub fn pre_arm_deadlines(input: &NotifyInput, now: Timestamp) -> BTreeMap<AlertClass, Timestamp> {
    let mut armed = BTreeMap::new();
    for class in AlertClass::ALL {
        if !class.pre_armable() {
            continue;
        }
        if let Some(at) = pre_arm_deadline(input, class)
            && at > now
        {
            armed.insert(class, at);
        }
    }
    // A timer armed for the wrong instant, or cancelled when it should not have been, is
    // otherwise invisible until an alert fails to arrive weeks later.
    tracing::debug!(
        now = %now,
        armed = ?armed
            .iter()
            .map(|(class, at)| (class.as_str(), at.to_string()))
            .collect::<Vec<_>>(),
        "notify: computed the pre-arm deadlines"
    );
    armed
}

/// The earliest instant in [`pre_arm_deadlines()`], or `None` when nothing is to be armed.
///
/// A convenience for a caller that holds a single timer — a desktop scheduler, a CLI, a status
/// line. **A client that pre-arms per class wants [`pre_arm_deadlines()`]**: collapsing the map
/// to its minimum discards the later class's timer, which on a device the app never runs on
/// again loses that alert.
#[must_use]
pub fn next_deadline(input: &NotifyInput, now: Timestamp) -> Option<Timestamp> {
    pre_arm_deadlines(input, now).into_values().min()
}

/// The predicate for one class. Separated per class rather than per fact so the emission order
/// is the enum's and the two quota classes stay independently suppressible.
fn evaluate_class(input: &NotifyInput, class: AlertClass, now: Timestamp) -> Option<Alert> {
    match class {
        AlertClass::SyncStale => sync_stale(input.sync.as_ref()?, now),
        AlertClass::RecoveryCheckDue => recovery_check_due(input.recovery.as_ref()?, now),
        AlertClass::QuotaSoft => {
            (input.quota?.state == QuotaAdvisory::SoftWarning).then(|| Alert::new(class))
        }
        AlertClass::QuotaGraceExpiring => quota_grace_expiring(input.quota?.state),
        AlertClass::QuarantinePending => counted(class, input.quarantine_pending),
        AlertClass::DropPending => counted(class, input.drops_pending),
    }
}

/// A class that fires on any non-zero count and carries it as the `count` parameter.
///
/// A quarantined item is never silently dropped and never silently applied, so any non-zero
/// count is reported. The two counts are **disjoint**: a pending drop is a quarantine surface in
/// the threat model's inventory, but it has its own class here, so
/// [`NotifyInput::quarantine_pending`] excludes drops and only
/// [`NotifyInput::drops_pending`] counts them.
fn counted(class: AlertClass, count: u64) -> Option<Alert> {
    (count > 0).then(|| Alert::new(class).with_param("count", count.to_string()))
}

/// "After two weeks without a completed sync *while changes remain un-synced*."
///
/// Both halves are load-bearing: with nothing un-synced the library is not behind, so a device
/// that simply has not needed to sync raises nothing.
fn sync_stale(facts: &SyncFacts, now: Timestamp) -> Option<Alert> {
    if facts.unsynced_changes == 0 {
        return None;
    }
    let deadline = sync_stale_deadline(facts);
    if now < deadline {
        return None;
    }
    // Clock skew backwards is not a fire: `now < deadline` already returned above, so the
    // subtraction here cannot go negative — but it saturates regardless rather than trusting it.
    let days_behind = now
        .as_second()
        .saturating_sub(facts.last_completed_sync.as_second())
        / DAY_SECS;
    Some(
        Alert::new(AlertClass::SyncStale)
            .with_deadline(deadline)
            .with_param("count", facts.unsynced_changes.to_string())
            .with_param("days_behind", days_behind.to_string()),
    )
}

/// When the staleness alert becomes true, given the sync epoch.
fn sync_stale_deadline(facts: &SyncFacts) -> Timestamp {
    add_secs(facts.last_completed_sync, SYNC_STALE_SECS)
}

/// The verification prompt is due, and no active snooze is holding it back.
///
/// The cadence ladder and the snooze accounting stay with the scheduler; this consumes
/// `next_due` as given. `snooze_budget_spent` does not change *whether* the class is reported —
/// a client cannot render a badge for a condition it was not told about — only how, which is
/// delivery and therefore the client's. It is carried as the `snooze_budget` parameter.
///
/// The reported `deadline` is the **later** of `next_due` and an expired `snoozed_until`,
/// because that is the instant whose passing actually made the alert true — and it is the same
/// instant [`pre_arm_deadline`] armed, so what a client scheduled and what it is handed on
/// arrival agree.
fn recovery_check_due(facts: &RecoveryFacts, now: Timestamp) -> Option<Alert> {
    if facts.snoozed_until.is_some_and(|until| until > now) {
        return None;
    }
    if now < facts.next_due {
        return None;
    }
    let became_true_at = facts
        .snoozed_until
        .map_or(facts.next_due, |until| until.max(facts.next_due));
    Some(
        Alert::new(AlertClass::RecoveryCheckDue)
            .with_deadline(became_true_at)
            .with_param(
                "snooze_budget",
                if facts.snooze_budget_spent {
                    "spent"
                } else {
                    "available"
                },
            )
            // Without this the alert for "you told us you lost your recovery secret" is
            // byte-identical to the routine ninety-day check, and a client rendering from the
            // class alone would say "time for your periodic check" at the worst moment.
            .with_param(
                "recovery",
                if facts.rewrap_due { "rewrap" } else { "check" },
            ),
    )
}

/// "Entering the grace window raises `quota_grace_expiring`."
///
/// `GraceExpired` raises the same class with `grace = "expired"` rather than going silent: it is
/// strictly additive to `HardExceeded` (metadata-growth writes are now refused too), so a device
/// that first polls after the window closed must still hear about it, and the class set is closed
/// — there is no `quota_grace_expired` to escalate into.
fn quota_grace_expiring(state: QuotaAdvisory) -> Option<Alert> {
    let grace = match state {
        QuotaAdvisory::HardExceeded => "counting",
        QuotaAdvisory::GraceExpired => "expired",
        // `Ok`/`SoftWarning` are below the hard limit; `Suspended` is an admin or billing
        // action owned by moderation, not a quota threshold.
        QuotaAdvisory::Ok | QuotaAdvisory::SoftWarning | QuotaAdvisory::Suspended => return None,
    };
    Some(Alert::new(AlertClass::QuotaGraceExpiring).with_param("grace", grace))
}

/// The instant to arm for one pre-armable class, if it has one that will certainly fire.
///
/// Two composition rules, both of which exist because an armed notification cannot be
/// re-checked when it fires:
///
/// - the class's own condition instant is the **later** of every gate on it — a recovery check
///   snoozed to a date *before* its due date is still not due until `next_due`, so arming the
///   snooze end alone would fire into no alert;
/// - a [snooze](NotifyInput::suppressed) **defers** that instant instead of cancelling it, so a
///   class snoozed after firing re-fires when the snooze ends. Only a
///   [disable](NotifyInput::disabled) cancels.
fn pre_arm_deadline(input: &NotifyInput, class: AlertClass) -> Option<Timestamp> {
    let condition_at = match class {
        AlertClass::SyncStale => input
            .sync
            .as_ref()
            .filter(|facts| facts.unsynced_changes > 0)
            .map(sync_stale_deadline),
        AlertClass::RecoveryCheckDue => input
            .recovery
            .as_ref()
            .filter(|facts| !facts.snooze_budget_spent)
            .map(|facts| {
                facts
                    .snoozed_until
                    .map_or(facts.next_due, |until| until.max(facts.next_due))
            }),
        // Not pre-armable; `pre_arm_deadlines` filters these out before asking.
        AlertClass::QuotaSoft
        | AlertClass::QuotaGraceExpiring
        | AlertClass::QuarantinePending
        | AlertClass::DropPending => None,
    }?;
    if input.disabled.contains(&class) {
        // Disabled: nothing is ever armed again for this class.
        return None;
    }
    // Snoozed: re-fire when the snooze ends, if that outlasts the condition itself.
    Some(match input.suppressed.get(&class) {
        Some(&until) => condition_at.max(until),
        None => condition_at,
    })
}

/// Add a signed second offset to a timestamp, saturating at the representable bounds. Nothing
/// here operates near them; saturation just keeps the arithmetic total, so a disabled class
/// pinned at [`Timestamp::MAX`] can never panic the predicate.
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
    use std::collections::{BTreeMap, BTreeSet};

    use super::super::class::AlertSeverity;
    use super::super::input::QuotaFacts;
    use super::*;

    /// A fixed, round base instant well away from the timestamp bounds.
    const BASE: i64 = 1_700_000_000;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    /// The classes `evaluate` reported, in order.
    fn classes(input: &NotifyInput, now: Timestamp) -> Vec<AlertClass> {
        evaluate(input, now).into_iter().map(|a| a.class).collect()
    }

    /// The `params` of the single alert of `class`, or `None` if it did not fire.
    fn params_of(
        input: &NotifyInput,
        class: AlertClass,
        now: Timestamp,
    ) -> Option<BTreeMap<String, String>> {
        evaluate(input, now)
            .into_iter()
            .find(|a| a.class == class)
            .map(|a| a.params)
    }

    fn with_sync(last_completed_sync: i64, unsynced_changes: u64) -> NotifyInput {
        NotifyInput {
            sync: Some(SyncFacts {
                last_completed_sync: ts(last_completed_sync),
                unsynced_changes,
            }),
            ..NotifyInput::default()
        }
    }

    fn with_recovery(
        next_due: i64,
        snoozed_until: Option<i64>,
        snooze_budget_spent: bool,
    ) -> NotifyInput {
        NotifyInput {
            recovery: Some(RecoveryFacts {
                next_due: ts(next_due),
                snoozed_until: snoozed_until.map(ts),
                snooze_budget_spent,
                rewrap_due: false,
            }),
            ..NotifyInput::default()
        }
    }

    fn with_quota(state: QuotaAdvisory) -> NotifyInput {
        NotifyInput {
            quota: Some(QuotaFacts { state }),
            ..NotifyInput::default()
        }
    }

    // ── sync_stale ──────────────────────────────────────────────────────────

    /// SSoT: "After two weeks without a completed sync *while changes remain un-synced*."
    /// Table-driven over the boundary instant and the un-synced half.
    #[test]
    fn sync_stale_fires_at_two_weeks_and_not_before() {
        let due = BASE + SYNC_STALE_SECS;
        let cases: &[(i64, u64, bool, &str)] = &[
            (due - 1, 1, false, "one second before the threshold"),
            (due, 1, true, "exactly at the threshold"),
            (due + DAY_SECS, 1, true, "a day past the threshold"),
            (due, 0, false, "at the threshold with nothing un-synced"),
            (
                due + 365 * DAY_SECS,
                0,
                false,
                "a year past, with nothing un-synced",
            ),
            (BASE, 5, false, "the instant the sync completed"),
            (BASE - DAY_SECS, 5, false, "clock skewed behind the epoch"),
        ];
        for &(now, unsynced, expected, why) in cases {
            let input = with_sync(BASE, unsynced);
            assert_eq!(
                classes(&input, ts(now)).contains(&AlertClass::SyncStale),
                expected,
                "{why}"
            );
        }
    }

    /// A device that has never completed a sync raises nothing: the alert is about a *stale*
    /// sync, not a missing one.
    #[test]
    fn sync_stale_needs_a_completed_sync_to_be_stale_from() {
        let input = NotifyInput::default();
        assert!(evaluate(&input, ts(BASE + 10 * SYNC_STALE_SECS)).is_empty());
        assert_eq!(next_deadline(&input, ts(BASE)), None);
    }

    /// The alert carries the deadline that produced it and the two catalog parameters.
    #[test]
    fn sync_stale_carries_deadline_and_params() {
        let input = with_sync(BASE, 42);
        let now = ts(BASE + SYNC_STALE_SECS + 3 * DAY_SECS);
        let alert = evaluate(&input, now)
            .into_iter()
            .find(|a| a.class == AlertClass::SyncStale)
            .expect("stale after 17 days with changes pending");

        assert_eq!(alert.severity, AlertSeverity::Warning);
        assert_eq!(alert.deadline, Some(ts(BASE + SYNC_STALE_SECS)));
        assert_eq!(alert.params["count"], "42");
        assert_eq!(alert.params["days_behind"], "17");
    }

    // ── recovery_check_due ──────────────────────────────────────────────────

    /// Due at the boundary; an active snooze holds it back; a snooze that has expired does not.
    #[test]
    fn recovery_check_due_boundaries() {
        let due = BASE + 7 * DAY_SECS;
        let cases: &[(i64, Option<i64>, bool, &str)] = &[
            (due - 1, None, false, "one second before due"),
            (due, None, true, "exactly at due"),
            (due + 1, None, true, "one second after due"),
            (due, Some(due + 1), false, "snoozed one second past now"),
            (due, Some(due), true, "snooze expiring exactly at now"),
            (due, Some(due - 1), true, "snooze already expired"),
            (due - 1, Some(due + 1), false, "snoozed and not yet due"),
        ];
        for &(now, snoozed, expected, why) in cases {
            let input = with_recovery(due, snoozed, false);
            assert_eq!(
                classes(&input, ts(now)).contains(&AlertClass::RecoveryCheckDue),
                expected,
                "{why}"
            );
        }
    }

    /// The reported deadline is the instant that actually made the alert true — the expired
    /// snooze when one outlasted the due date, and the due date otherwise. It is the same
    /// instant `next_deadline` armed, so a client's timer and the alert it is handed agree.
    #[test]
    fn recovery_check_due_reports_the_instant_that_made_it_true() {
        let due = BASE + 7 * DAY_SECS;
        let until = due + 2 * DAY_SECS;

        // A snooze that outlasted the due date: it, not `next_due`, is what held the alert.
        let input = with_recovery(due, Some(until), false);
        assert_eq!(
            next_deadline(&input, ts(due)),
            Some(ts(until)),
            "the snooze end is what gets armed"
        );
        let alert = evaluate(&input, ts(until))
            .into_iter()
            .find(|a| a.class == AlertClass::RecoveryCheckDue)
            .expect("due once the snooze expires");
        assert_eq!(alert.deadline, Some(ts(until)), "and what gets reported");

        // A snooze that expired before the due date leaves `next_due` as the true instant.
        let early = with_recovery(due, Some(due - DAY_SECS), false);
        let alert = evaluate(&early, ts(due))
            .into_iter()
            .find(|a| a.class == AlertClass::RecoveryCheckDue)
            .expect("due at the boundary");
        assert_eq!(alert.deadline, Some(ts(due)));

        // No snooze at all: `next_due`.
        let none = with_recovery(due, None, false);
        let alert = evaluate(&none, ts(due))
            .into_iter()
            .find(|a| a.class == AlertClass::RecoveryCheckDue)
            .expect("due at the boundary");
        assert_eq!(alert.deadline, Some(ts(due)));
    }

    /// The two halves of the bounded-snooze rule, which live at different layers.
    ///
    /// "Past that bound the alert stops re-firing and degrades to a persistent, non-blocking
    /// badge": the *stops re-firing* half is enforced here at the pre-arm layer — with the
    /// budget spent, no timer is armed for the class, ever. The *badge* half needs the opposite:
    /// `evaluate` keeps reporting the class, because a client cannot render a badge for a
    /// condition it was never told about. Suppressing the class instead would satisfy the first
    /// sentence by making the second impossible.
    #[test]
    fn a_spent_snooze_budget_stops_the_timer_but_not_the_report() {
        let due = BASE + 7 * DAY_SECS;
        for (now, why) in [
            (due - DAY_SECS, "before the due date"),
            (due, "at the due date"),
            (due + 30 * DAY_SECS, "long after it"),
        ] {
            let spent = with_recovery(due, None, true);
            assert!(
                pre_arm_deadlines(&spent, ts(now)).is_empty(),
                "no timer is armed {why}"
            );
            assert_eq!(next_deadline(&spent, ts(now)), None, "{why}");
        }

        // ...and the class is still reported once due, carrying the fact that makes it a badge.
        let spent = with_recovery(due, None, true);
        let params = params_of(&spent, AlertClass::RecoveryCheckDue, ts(due))
            .expect("a spent budget degrades the presentation, it does not silence the class");
        assert_eq!(params["snooze_budget"], "spent");

        // A snooze still running with the budget spent arms nothing either: the class is
        // already a badge, and a badge never escalates back into an alert on its own.
        let snoozed_and_spent = with_recovery(due, Some(due + DAY_SECS), true);
        assert!(pre_arm_deadlines(&snoozed_and_spent, ts(due)).is_empty());
    }

    /// A spent snooze budget is reported, not silenced — the client needs the fact to render a
    /// badge — and it is carried as a parameter rather than a class of its own.
    #[test]
    fn recovery_check_due_reports_the_snooze_budget() {
        let due = BASE + 7 * DAY_SECS;
        for (spent, expected) in [(false, "available"), (true, "spent")] {
            let input = with_recovery(due, None, spent);
            let params = params_of(&input, AlertClass::RecoveryCheckDue, ts(due))
                .expect("due at the boundary regardless of the budget");
            assert_eq!(params["snooze_budget"], expected);
        }
    }

    // ── quota ───────────────────────────────────────────────────────────────

    /// Each quota state maps to exactly the classes the SSoT's table names, and no more.
    #[test]
    fn quota_states_map_to_their_classes() {
        let cases: &[(QuotaAdvisory, &[AlertClass], Option<&str>)] = &[
            (QuotaAdvisory::Ok, &[], None),
            (QuotaAdvisory::SoftWarning, &[AlertClass::QuotaSoft], None),
            (
                QuotaAdvisory::HardExceeded,
                &[AlertClass::QuotaGraceExpiring],
                Some("counting"),
            ),
            (
                QuotaAdvisory::GraceExpired,
                &[AlertClass::QuotaGraceExpiring],
                Some("expired"),
            ),
            (QuotaAdvisory::Suspended, &[], None),
        ];
        for &(state, expected, grace) in cases {
            let input = with_quota(state);
            assert_eq!(classes(&input, ts(BASE)), expected, "{state:?}");
            if let Some(grace) = grace {
                let params = params_of(&input, AlertClass::QuotaGraceExpiring, ts(BASE))
                    .expect("the grace class fired");
                assert_eq!(params["grace"], grace, "{state:?}");
            }
        }
    }

    /// Quota alerts carry no deadline: the wire response has no `over_since`, so the device
    /// cannot compute one, which is why the class is not pre-armable.
    #[test]
    fn quota_alerts_are_not_pre_armable() {
        for state in [QuotaAdvisory::SoftWarning, QuotaAdvisory::GraceExpired] {
            let input = with_quota(state);
            for alert in evaluate(&input, ts(BASE)) {
                assert_eq!(alert.deadline, None, "{state:?}");
            }
            assert_eq!(next_deadline(&input, ts(BASE)), None, "{state:?}");
        }
    }

    // ── counted classes ─────────────────────────────────────────────────────

    /// A quarantined item is never silently dropped and never silently applied: any non-zero
    /// count is reported, with the count as a parameter.
    #[test]
    fn counted_classes_fire_on_any_non_zero_count() {
        let cases: &[(u64, u64, &[AlertClass])] = &[
            (0, 0, &[]),
            (1, 0, &[AlertClass::QuarantinePending]),
            (0, 1, &[AlertClass::DropPending]),
            (
                3,
                7,
                &[AlertClass::QuarantinePending, AlertClass::DropPending],
            ),
        ];
        for &(quarantine, drops, expected) in cases {
            let input = NotifyInput {
                quarantine_pending: quarantine,
                drops_pending: drops,
                ..NotifyInput::default()
            };
            assert_eq!(classes(&input, ts(BASE)), expected);
        }

        let input = NotifyInput {
            quarantine_pending: 3,
            drops_pending: 7,
            ..NotifyInput::default()
        };
        assert_eq!(
            params_of(&input, AlertClass::QuarantinePending, ts(BASE)).unwrap()["count"],
            "3"
        );
        assert_eq!(
            params_of(&input, AlertClass::DropPending, ts(BASE)).unwrap()["count"],
            "7"
        );
    }

    // ── suppression ─────────────────────────────────────────────────────────

    /// Suppression is per class, exclusive at the instant, and removes the class from both the
    /// report and the arm decision.
    #[test]
    fn suppression_boundaries_apply_to_alerts_and_deadlines() {
        let due = BASE + SYNC_STALE_SECS;
        let cases: &[(i64, bool, &str)] = &[
            (due + 1, false, "suppressed past now"),
            (due, true, "suppression expiring exactly at now"),
            (due - 1, true, "suppression already expired"),
        ];
        for &(until, expected, why) in cases {
            let mut input = with_sync(BASE, 1);
            input.suppressed.insert(AlertClass::SyncStale, ts(until));
            assert_eq!(
                classes(&input, ts(due)).contains(&AlertClass::SyncStale),
                expected,
                "{why}"
            );
        }

        // A disabled class contributes no deadline even while its own is still in the future.
        let mut input = with_sync(BASE, 1);
        input.disabled.insert(AlertClass::SyncStale);
        assert_eq!(next_deadline(&input, ts(BASE)), None);
        assert!(evaluate(&input, ts(due)).is_empty());
    }

    /// Disabling suppresses the warning, never the behavior — so the *other* classes are
    /// untouched by one class's disable.
    #[test]
    fn suppression_does_not_leak_across_classes() {
        let mut input = with_sync(BASE, 1);
        input.drops_pending = 2;
        input.disabled.insert(AlertClass::SyncStale);
        assert_eq!(
            classes(&input, ts(BASE + SYNC_STALE_SECS)),
            [AlertClass::DropPending]
        );
    }

    // ── next_deadline ───────────────────────────────────────────────────────

    /// The earlier of the two pre-armable deadlines wins, and only future ones count.
    #[test]
    fn next_deadline_is_the_earliest_future_pre_armable_instant() {
        let sync_due = BASE + SYNC_STALE_SECS; // BASE + 14 d
        let recovery_due = BASE + 7 * DAY_SECS; // earlier

        let mut input = with_sync(BASE, 1);
        input.recovery = Some(RecoveryFacts {
            next_due: ts(recovery_due),
            snoozed_until: None,
            snooze_budget_spent: false,
            rewrap_due: false,
        });

        // Both future → the earlier.
        assert_eq!(next_deadline(&input, ts(BASE)), Some(ts(recovery_due)));
        // The recovery deadline has passed → the staleness one.
        assert_eq!(
            next_deadline(&input, ts(recovery_due)),
            Some(ts(sync_due)),
            "a deadline exactly at now has already fired and is not re-armed"
        );
        // Both passed → nothing to arm; the client cancels its timer.
        assert_eq!(next_deadline(&input, ts(sync_due)), None);
        assert_eq!(next_deadline(&input, ts(sync_due + DAY_SECS)), None);
    }

    /// While snoozed, the armed instant is the snooze's end rather than the original due date.
    #[test]
    fn snooze_moves_the_armed_instant() {
        let due = BASE + 7 * DAY_SECS;
        let until = due + 2 * DAY_SECS;
        let input = with_recovery(due, Some(until), false);
        assert_eq!(next_deadline(&input, ts(BASE)), Some(ts(until)));
        assert_eq!(next_deadline(&input, ts(due)), Some(ts(until)));
        // Once the snooze has expired the class is due, so there is nothing left to arm.
        assert_eq!(next_deadline(&input, ts(until)), None);
    }

    /// An armed notification cannot be re-checked when it fires, so nothing is armed that would
    /// arrive as a badge or as no alert at all.
    #[test]
    fn nothing_is_armed_that_would_arrive_empty() {
        // Nothing un-synced: only a sync can change that, and a sync re-arms.
        assert_eq!(next_deadline(&with_sync(BASE, 0), ts(BASE)), None);
        // Snooze budget spent: the class has degraded to a badge, which is in-app, not a timer.
        let due = BASE + 7 * DAY_SECS;
        assert_eq!(
            next_deadline(&with_recovery(due, None, true), ts(BASE)),
            None
        );
        // ...while the same facts with budget left do arm.
        assert_eq!(
            next_deadline(&with_recovery(due, None, false), ts(BASE)),
            Some(ts(due))
        );
    }

    // ── whole-surface properties ────────────────────────────────────────────

    /// A client that has learned nothing reports nothing and arms nothing.
    #[test]
    fn default_input_is_silent() {
        let input = NotifyInput::default();
        assert!(evaluate(&input, ts(BASE)).is_empty());
        assert_eq!(next_deadline(&input, ts(BASE)), None);
    }

    /// Emission order is the enum's, and every class can be true at once.
    #[test]
    fn every_class_can_fire_together_in_enum_order() {
        let due = BASE + SYNC_STALE_SECS;
        let input = NotifyInput {
            sync: Some(SyncFacts {
                last_completed_sync: ts(BASE),
                unsynced_changes: 1,
            }),
            recovery: Some(RecoveryFacts {
                next_due: ts(BASE),
                snoozed_until: None,
                snooze_budget_spent: false,
                rewrap_due: false,
            }),
            // `SoftWarning` and `HardExceeded` are mutually exclusive states, so the two quota
            // classes cannot both be true; this asserts the ordering of the five that can.
            quota: Some(QuotaFacts {
                state: QuotaAdvisory::HardExceeded,
            }),
            quarantine_pending: 1,
            drops_pending: 1,
            suppressed: BTreeMap::new(),
            disabled: BTreeSet::new(),
        };
        assert_eq!(
            classes(&input, ts(due)),
            [
                AlertClass::SyncStale,
                AlertClass::RecoveryCheckDue,
                AlertClass::QuotaGraceExpiring,
                AlertClass::QuarantinePending,
                AlertClass::DropPending,
            ]
        );
    }

    /// Determinism: equal input gives byte-equal output through `serde`, which is what
    /// `BTreeMap` params and a fixed emission order buy.
    #[test]
    fn evaluation_is_deterministic_through_serde() {
        let mut input = with_sync(BASE, 9);
        input.quarantine_pending = 2;
        input.drops_pending = 4;
        input.disabled.insert(AlertClass::RecoveryCheckDue);
        let now = ts(BASE + SYNC_STALE_SECS);

        let first = serde_json::to_string(&evaluate(&input, now)).unwrap();
        let second = serde_json::to_string(&evaluate(&input, now)).unwrap();
        assert_eq!(first, second);

        // And the input itself round-trips, so a persisted snapshot re-evaluates identically.
        let round_tripped: NotifyInput =
            serde_json::from_str(&serde_json::to_string(&input).unwrap()).unwrap();
        assert_eq!(round_tripped, input);
        assert_eq!(evaluate(&round_tripped, now), evaluate(&input, now));
    }

    // ── per-class arming, deferral, and the new facts ───────────────────────

    /// The two pre-armable classes get **independent** entries. Collapsing them to a single
    /// minimum, as `next_deadline` does, discards the later timer — which on a device the app
    /// never runs on again loses that alert entirely.
    #[test]
    fn pre_arm_deadlines_arms_each_class_independently() {
        let sync_due = BASE + SYNC_STALE_SECS; // + 14 d
        let recovery_due = BASE + 90 * DAY_SECS; // much later
        let mut input = with_sync(BASE, 1);
        input.recovery = Some(RecoveryFacts {
            next_due: ts(recovery_due),
            snoozed_until: None,
            snooze_budget_spent: false,
            rewrap_due: false,
        });

        let armed = pre_arm_deadlines(&input, ts(BASE));
        assert_eq!(armed.len(), 2);
        assert_eq!(armed[&AlertClass::SyncStale], ts(sync_due));
        assert_eq!(armed[&AlertClass::RecoveryCheckDue], ts(recovery_due));
        // The single-timer convenience keeps only the earlier one, which is precisely why it is
        // not what a per-class client should call.
        assert_eq!(next_deadline(&input, ts(BASE)), Some(ts(sync_due)));

        // Past the staleness deadline the recovery timer is still armed and still later.
        let armed = pre_arm_deadlines(&input, ts(sync_due));
        assert_eq!(
            armed.keys().copied().collect::<Vec<_>>(),
            [AlertClass::RecoveryCheckDue]
        );
        assert_eq!(armed[&AlertClass::RecoveryCheckDue], ts(recovery_due));
    }

    /// A class snoozed *after* it fired must fire again when the snooze ends. Cancelling
    /// instead would leave `sync_stale` reachable only in-app, defeating the pre-arm rule for
    /// the one class it exists for.
    #[test]
    fn a_finite_suppression_defers_the_timer_rather_than_cancelling_it() {
        let due = BASE + SYNC_STALE_SECS;
        let snooze_end = due + 3 * DAY_SECS;
        let mut input = with_sync(BASE, 1);
        input
            .suppressed
            .insert(AlertClass::SyncStale, ts(snooze_end));

        // Snoozed: nothing is reported, but the timer moves to the snooze end.
        assert!(evaluate(&input, ts(due + DAY_SECS)).is_empty());
        assert_eq!(
            pre_arm_deadlines(&input, ts(due + DAY_SECS))[&AlertClass::SyncStale],
            ts(snooze_end)
        );
        // At the snooze end it is due again, and there is nothing left to arm.
        assert!(
            classes(&input, ts(snooze_end)).contains(&AlertClass::SyncStale),
            "the snooze has expired, so the class is due"
        );
        assert!(pre_arm_deadlines(&input, ts(snooze_end)).is_empty());

        // A snooze that ends before the class's own deadline does not pull the timer earlier.
        let mut early = with_sync(BASE, 1);
        early
            .suppressed
            .insert(AlertClass::SyncStale, ts(due - DAY_SECS));
        assert_eq!(
            pre_arm_deadlines(&early, ts(BASE))[&AlertClass::SyncStale],
            ts(due)
        );
    }

    /// A disable is the one suppression that cancels the timer: nothing is ever armed again.
    #[test]
    fn a_disabled_class_holds_no_timer_and_reports_nothing() {
        let mut input = with_sync(BASE, 1);
        input.disabled.insert(AlertClass::SyncStale);
        assert!(pre_arm_deadlines(&input, ts(BASE)).is_empty());
        assert!(pre_arm_deadlines(&input, ts(BASE + SYNC_STALE_SECS)).is_empty());
        assert!(evaluate(&input, ts(BASE + SYNC_STALE_SECS)).is_empty());
    }

    /// A snooze set before the due date does not make the check due earlier, so it must not be
    /// armed alone: the armed instant is the later of the two, or the timer fires into no alert.
    #[test]
    fn a_snooze_before_the_due_date_does_not_pull_the_recovery_timer_earlier() {
        let due = BASE + 7 * DAY_SECS;
        let snooze_end = BASE + DAY_SECS; // expires long before the check is due
        let input = with_recovery(due, Some(snooze_end), false);

        assert_eq!(
            pre_arm_deadlines(&input, ts(BASE))[&AlertClass::RecoveryCheckDue],
            ts(due)
        );
        assert!(
            evaluate(&input, ts(snooze_end)).is_empty(),
            "the snooze ended but the check is not due yet"
        );
        assert!(classes(&input, ts(due)).contains(&AlertClass::RecoveryCheckDue));
    }

    /// The re-arm half of the rule: a completed sync moves the staleness timer, and a client
    /// that recomputes sees a different value to cancel-and-arm against.
    #[test]
    fn a_completed_sync_re_arms_the_staleness_timer() {
        let first = with_sync(BASE, 1);
        let before = pre_arm_deadlines(&first, ts(BASE))[&AlertClass::SyncStale];
        assert_eq!(before, ts(BASE + SYNC_STALE_SECS));

        // A sync completes a day later with changes still pending: the deadline moves by a day.
        let second = with_sync(BASE + DAY_SECS, 1);
        let after = pre_arm_deadlines(&second, ts(BASE + DAY_SECS))[&AlertClass::SyncStale];
        assert_eq!(after, ts(BASE + DAY_SECS + SYNC_STALE_SECS));
        assert_ne!(before, after, "the value moved, so the client re-arms");

        // A sync that clears the backlog cancels it instead.
        let cleared = with_sync(BASE + DAY_SECS, 0);
        assert!(pre_arm_deadlines(&cleared, ts(BASE + DAY_SECS)).is_empty());
    }

    /// The escalation to the guided re-wrap is carried as a parameter, because the closed class
    /// set has only `recovery_check_due` to report it and the routine check must not look the
    /// same.
    #[test]
    fn the_rewrap_escalation_is_distinguishable_from_a_routine_check() {
        let due = BASE + 7 * DAY_SECS;
        let routine = with_recovery(due, None, false);
        assert_eq!(
            params_of(&routine, AlertClass::RecoveryCheckDue, ts(due)).unwrap()["recovery"],
            "check"
        );

        let mut escalated = routine.clone();
        if let Some(facts) = escalated.recovery.as_mut() {
            facts.rewrap_due = true;
        }
        assert_eq!(
            params_of(&escalated, AlertClass::RecoveryCheckDue, ts(due)).unwrap()["recovery"],
            "rewrap"
        );
    }

    /// `days_behind` truncates toward the completed day, so the whole last day before the next
    /// one reads the same. Asserted at both ends of that interval.
    #[test]
    fn days_behind_truncates_to_whole_days() {
        for (offset, expected) in [
            (SYNC_STALE_SECS, "14"),
            (SYNC_STALE_SECS + DAY_SECS - 1, "14"),
            (SYNC_STALE_SECS + DAY_SECS, "15"),
        ] {
            let input = with_sync(BASE, 1);
            let params = params_of(&input, AlertClass::SyncStale, ts(BASE + offset))
                .expect("stale with changes pending");
            assert_eq!(params["days_behind"], expected, "at +{offset}s");
        }
    }

    /// `quota_soft` carries no parameters: there is nothing to interpolate that the client does
    /// not already hold from its own quota response.
    #[test]
    fn quota_soft_carries_no_parameters() {
        let input = with_quota(QuotaAdvisory::SoftWarning);
        let params = params_of(&input, AlertClass::QuotaSoft, ts(BASE)).expect("soft warning");
        assert!(params.is_empty(), "{params:?}");
    }

    /// Saturating arithmetic: a sync epoch pinned at the far end of the range neither panics
    /// nor wraps into the past.
    #[test]
    fn deadlines_saturate_at_the_representable_bounds() {
        let input = NotifyInput {
            sync: Some(SyncFacts {
                last_completed_sync: Timestamp::MAX,
                unsynced_changes: 1,
            }),
            ..NotifyInput::default()
        };
        assert!(evaluate(&input, ts(BASE)).is_empty());
        assert_eq!(next_deadline(&input, ts(BASE)), Some(Timestamp::MAX));

        let ancient = NotifyInput {
            sync: Some(SyncFacts {
                last_completed_sync: Timestamp::MIN,
                unsynced_changes: 1,
            }),
            ..NotifyInput::default()
        };
        assert!(
            classes(&ancient, ts(BASE)).contains(&AlertClass::SyncStale),
            "an epoch at the far past is long stale"
        );
        assert_eq!(next_deadline(&ancient, ts(BASE)), None);
    }
}
