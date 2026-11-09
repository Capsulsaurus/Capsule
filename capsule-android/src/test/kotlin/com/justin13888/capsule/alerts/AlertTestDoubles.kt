package com.justin13888.capsule.alerts

import java.time.Instant

/** 2026-01-01T00:00:00Z. */
val EPOCH: Instant = Instant.ofEpochSecond(1_767_225_600)

fun day(
    days: Long,
    plusSeconds: Long = 0,
): Instant = EPOCH.plusSeconds(days * NotifyFacts.SECONDS_PER_DAY + plusSeconds)

/**
 * The core's documented rules for the two pre-armable classes and the counted ones -
 * enough to drive the lifecycle. The real app calls the core through [FfiAlertPolicy].
 */
class RulesPolicy : AlertPolicy {
    var refuse = false

    /** The core cannot be reached, as [FfiAlertPolicy] reports a missing native library. */
    var unavailable = false

    override fun evaluate(
        facts: NotifyFacts,
        now: Instant,
    ): List<Alert> {
        require(!refuse) { "refused" }
        check(!unavailable) { "unavailable" }
        return AlertClass.entries.mapNotNull { alertClass ->
            val until = facts.suppressedUntil[alertClass]
            when {
                alertClass in facts.disabled -> null
                until != null && until > now -> null
                else -> alert(alertClass, facts, now)
            }
        }
    }

    override fun preArmDeadlines(
        facts: NotifyFacts,
        now: Instant,
    ): List<ClassDeadline> {
        require(!refuse) { "refused" }
        check(!unavailable) { "unavailable" }
        return AlertClass.entries.mapNotNull { alertClass ->
            val condition = condition(alertClass, facts)
            if (!alertClass.isPreArmable || alertClass in facts.disabled || condition == null) return@mapNotNull null
            val at = facts.suppressedUntil[alertClass]?.let { maxOf(it, condition) } ?: condition
            if (at > now) ClassDeadline(alertClass, at) else null
        }
    }

    private fun condition(
        alertClass: AlertClass,
        facts: NotifyFacts,
    ): Instant? =
        when (alertClass) {
            AlertClass.SYNC_STALE ->
                facts.lastCompletedSync?.takeIf { facts.unsyncedChanges > 0 }?.plusSeconds(STALE_SECONDS)
            AlertClass.RECOVERY_CHECK_DUE ->
                facts.recoveryNextDue?.takeIf { !facts.recoverySnoozeBudgetSpent }
            else -> null
        }

    private fun alert(
        alertClass: AlertClass,
        facts: NotifyFacts,
        now: Instant,
    ): Alert? =
        when (alertClass) {
            AlertClass.SYNC_STALE -> {
                val deadline = facts.lastCompletedSync?.takeIf { facts.unsyncedChanges > 0 }?.plusSeconds(STALE_SECONDS)
                if (deadline != null && now >= deadline) Alert(alertClass, deadline) else null
            }
            AlertClass.RECOVERY_CHECK_DUE -> {
                val due = facts.recoveryNextDue
                if (due != null && now >= due) {
                    Alert(alertClass, due, mapOf(AlertParameter.RECOVERY to if (facts.recoveryRewrapDue) "rewrap" else "check"))
                } else {
                    null
                }
            }
            AlertClass.QUOTA_SOFT -> if (facts.quotaState == QuotaAdvisory.SOFT_WARNING) Alert(alertClass) else null
            AlertClass.QUARANTINE_PENDING -> if (facts.quarantinePending > 0) Alert(alertClass) else null
            else -> null
        }

    private companion object {
        const val STALE_SECONDS = 14 * NotifyFacts.SECONDS_PER_DAY
    }
}

class FakeScheduler(
    initial: List<ArmedAlarm> = emptyList(),
) : AlarmScheduler {
    private val alarms = initial.associateBy { it.alertClass }.toMutableMap()
    val log = mutableListOf<AlertTimerOperation>()

    override fun armed(): Map<AlertClass, ArmedAlarm> = alarms.toMap()

    override fun arm(alarm: ArmedAlarm) {
        alarms[alarm.alertClass] = alarm
        log += AlertTimerOperation.Arm(alarm)
    }

    override fun cancel(alertClass: AlertClass) {
        if (alarms.remove(alertClass) != null) log += AlertTimerOperation.Cancel(alertClass)
    }

    /** The most live alarms any one class ever held, replayed from [log]. */
    fun maximumLivePerClass(): Int {
        val live = mutableMapOf<AlertClass, Int>()
        var peak = 0
        for (operation in log) {
            when (operation) {
                is AlertTimerOperation.Arm -> live.merge(operation.alarm.alertClass, 1, Int::plus)
                is AlertTimerOperation.Cancel -> live.merge(operation.alertClass, -1, Int::plus)
            }
            peak = maxOf(peak, live.values.maxOrNull() ?: 0)
        }
        return peak
    }
}

/** A prompt the test answers by hand, as Android does: asynchronously. */
class FakePermission(
    var current: NotificationPermissionState = NotificationPermissionState.NOT_DETERMINED,
) : NotificationPermission {
    var requests = 0

    override fun state() = current

    override fun request() {
        requests += 1
    }
}

class InMemorySnoozes : AlertSnoozeStore {
    var ledger = AlertSnoozeLedger()

    override fun load() = ledger

    override fun save(ledger: AlertSnoozeLedger) {
        this.ledger = ledger
    }
}

class Harness(
    var facts: NotifyFacts = NotifyFacts(),
    var now: Instant = EPOCH,
    val permission: FakePermission = FakePermission(NotificationPermissionState.GRANTED),
    val scheduler: FakeScheduler = FakeScheduler(),
    val policy: RulesPolicy = RulesPolicy(),
) {
    val snoozes = InMemorySnoozes()
    val coordinator = AlertCoordinator({ facts }, policy, scheduler, permission, snoozes) { now }

    fun armedAt(): Map<AlertClass, Instant> = scheduler.armed().mapValues { it.value.fireAt }
}
