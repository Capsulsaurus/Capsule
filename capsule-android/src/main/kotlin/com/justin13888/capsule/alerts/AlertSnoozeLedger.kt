package com.justin13888.capsule.alerts

import java.time.Instant

/** What a snooze request did. */
sealed interface AlertSnoozeOutcome {
    /** Suppressed until [until]; an armed alarm is deferred to it. */
    data class Snoozed(
        val until: Instant,
    ) : AlertSnoozeOutcome

    /** The budget is spent: a persistent in-app badge that never escalates back. */
    data object BadgeOnly : AlertSnoozeOutcome

    /** The recovery check is snoozed by its own cadence scheduler, not this ledger. */
    data object OwnedByRecoveryCadence : AlertSnoozeOutcome
}

/**
 * The bounded-snooze-then-badge state for the five classes the recovery cadence does
 * not own (Notifications - Shared Alert Mechanics). The core takes only the suppression
 * instant, so the count that bounds it lives here, once. A class's count resets when
 * its condition clears.
 */
data class AlertSnoozeLedger(
    val entries: Map<AlertClass, Entry> = emptyMap(),
) {
    /** One class's snooze end (exclusive) and consecutive snoozes since it last cleared. */
    data class Entry(
        val until: Instant,
        val count: Int,
    )

    /** Snooze [alertClass] from [now], returning the outcome and the next ledger. */
    fun snooze(
        alertClass: AlertClass,
        now: Instant,
    ): Pair<AlertSnoozeOutcome, AlertSnoozeLedger> {
        if (alertClass == AlertClass.RECOVERY_CHECK_DUE) return AlertSnoozeOutcome.OwnedByRecoveryCadence to this
        val taken = entries[alertClass]?.count ?: 0
        if (taken >= MAXIMUM_CONSECUTIVE_SNOOZES) return AlertSnoozeOutcome.BadgeOnly to this
        val until = now.plusSeconds(INTERVAL_SECONDS)
        return AlertSnoozeOutcome.Snoozed(until) to copy(entries = entries + (alertClass to Entry(until, taken + 1)))
    }

    /** The suppression instants to hand the predicate. */
    val suppressions: Map<AlertClass, Instant>
        get() = entries.mapValues { it.value.until }

    /** The classes whose budget is spent: shown in-app, never armed. */
    val badgeOnly: Set<AlertClass>
        get() = entries.filterValues { it.count >= MAXIMUM_CONSECUTIVE_SNOOZES }.keys

    /** Forget every class that is no longer true. */
    fun clear(keeping: Set<AlertClass>): AlertSnoozeLedger = copy(entries = entries.filterKeys { it in keeping })

    companion object {
        /** How long one snooze lasts. */
        const val INTERVAL_SECONDS = 86_400L

        /** Consecutive snoozes before badge-only - the recovery cadence's own bound. */
        const val MAXIMUM_CONSECUTIVE_SNOOZES = 3
    }
}
