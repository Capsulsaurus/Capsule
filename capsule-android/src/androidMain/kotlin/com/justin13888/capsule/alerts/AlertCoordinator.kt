package com.justin13888.capsule.alerts

import java.time.Instant
import java.util.logging.Level
import java.util.logging.Logger

/** What one pass of the alert loop did - for logs and tests. */
data class AlertRefreshOutcome(
    val delivery: Delivery,
    /** Every alert true now - the in-app badge set. */
    val badges: List<Alert>,
    val operations: List<AlertTimerOperation>,
) {
    enum class Delivery {
        /** Nothing needed arming, so permission was not consulted. */
        NOTHING_TO_ARM,

        /** Permission is granted; the alarms were reconciled. */
        ARMED,

        /** The prompt was started; the next refresh arms once it is answered. */
        AWAITING_PERMISSION,

        /** Permission is refused: badges only, every alarm cancelled. Not an error. */
        BADGES_ONLY,

        /** The predicate refused its input; the armed alarms were left as they were. */
        POLICY_FAILED,
    }
}

/**
 * The client half of local alerts: the per-class arm / re-arm / cancel loop,
 * permission at first use, and the bounded snooze.
 *
 * [refresh] is the only entry point and is idempotent: read the facts, ask the
 * predicate which classes are true (the badges) and when each pre-armable class must
 * fire (per class), then reconcile the OS alarms against that with [AlertTimerPlan].
 * Call it after any state change. The permission prompt is started the first time a
 * pass has something to arm; a refusal degrades every class to its in-app badge and is
 * never retried. Nothing here reaches sync, upload, or any other flow.
 */
class AlertCoordinator(
    private val facts: AlertFactsSource,
    private val policy: AlertPolicy,
    private val scheduler: AlarmScheduler,
    private val permission: NotificationPermission,
    private val snoozes: AlertSnoozeStore,
    private val clock: () -> Instant = Instant::now,
) {
    /** Every alert true at the last refresh. */
    @Volatile
    var badges: List<Alert> = emptyList()
        private set

    @Synchronized
    fun refresh(): AlertRefreshOutcome {
        val now = clock()
        val raw = facts.facts()
        var ledger = snoozes.load()
        val input = applying(ledger, raw)
        val reported: List<Alert>
        val deadlines: List<ClassDeadline>
        val stillTrue: Set<AlertClass>
        try {
            reported = policy.evaluate(input, now)
            deadlines = policy.preArmDeadlines(input, now)
            // Asked without this ledger's snoozes: a snoozed class reports nothing, and
            // reading that as "cleared" would refill its budget every snooze.
            stillTrue = policy.evaluate(input.copy(suppressedUntil = raw.suppressedUntil), now).map { it.alertClass }.toSet()
        } catch (error: IllegalArgumentException) {
            log.log(Level.WARNING, "alert predicate refused its input", error)
            return AlertRefreshOutcome(AlertRefreshOutcome.Delivery.POLICY_FAILED, badges, emptyList())
        }
        badges = reported
        val cleared = ledger.clear(stillTrue)
        if (cleared != ledger) {
            ledger = cleared
            snoozes.save(ledger)
        }

        val badgeOnly = ledger.badgeOnly
        val desired =
            deadlines
                .filter { it.alertClass !in badgeOnly }
                .map { ArmedAlarm(input.armedAlert(it), it.deadline) }
        log.fine {
            "alerts at $now: true=${reported.map { it.alertClass.wire }} " +
                "arm=${desired.map { "${it.alertClass.wire}@${it.fireAt}" }} badgeOnly=${badgeOnly.map { it.wire }}"
        }

        val delivery =
            when {
                desired.isEmpty() -> AlertRefreshOutcome.Delivery.NOTHING_TO_ARM
                else ->
                    when (permission.state()) {
                        NotificationPermissionState.GRANTED -> AlertRefreshOutcome.Delivery.ARMED
                        NotificationPermissionState.DENIED -> AlertRefreshOutcome.Delivery.BADGES_ONLY
                        NotificationPermissionState.NOT_DETERMINED -> {
                            log.info("requesting notification permission: a class has something to arm")
                            permission.request()
                            AlertRefreshOutcome.Delivery.AWAITING_PERMISSION
                        }
                    }
            }
        // Awaiting an answer leaves the alarms as they are; a refusal clears them.
        val operations =
            when (delivery) {
                AlertRefreshOutcome.Delivery.AWAITING_PERMISSION -> emptyList()
                AlertRefreshOutcome.Delivery.ARMED -> AlertTimerPlan.reconcile(desired, scheduler.armed())
                else -> AlertTimerPlan.reconcile(emptyList(), scheduler.armed())
            }
        operations.forEach(::apply)
        log.info("alert refresh: delivery=$delivery badges=${reported.size} operations=${operations.size}")
        return AlertRefreshOutcome(delivery, reported, operations)
    }

    /**
     * Snooze [alertClass], then refresh so its alarm moves to the snooze end. Bounded:
     * once the budget is spent the class is badge-only.
     */
    @Synchronized
    fun snooze(alertClass: AlertClass): AlertSnoozeOutcome {
        val (outcome, next) = snoozes.load().snooze(alertClass, clock())
        if (outcome is AlertSnoozeOutcome.Snoozed) snoozes.save(next)
        log.info("snooze ${alertClass.wire}: $outcome")
        refresh()
        return outcome
    }

    private fun apply(operation: AlertTimerOperation) {
        when (operation) {
            is AlertTimerOperation.Cancel -> scheduler.cancel(operation.alertClass)
            is AlertTimerOperation.Arm -> scheduler.arm(operation.alarm)
        }
    }

    private companion object {
        val log: Logger = Logger.getLogger("capsule.alerts")

        /** The facts with the ledger's snoozes merged in; the later instant wins. */
        fun applying(
            ledger: AlertSnoozeLedger,
            facts: NotifyFacts,
        ): NotifyFacts {
            val merged = facts.suppressedUntil.toMutableMap()
            for ((alertClass, until) in ledger.suppressions) {
                merged[alertClass] = maxOf(until, merged[alertClass] ?: until)
            }
            return facts.copy(suppressedUntil = merged)
        }
    }
}
