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
/// [Download & Sync — Notifications]: https://docs/design/sync/#notifications
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
mod tests;
