package com.justin13888.capsule.alerts

import java.time.Instant

/**
 * One of the local alert classes - the closed set `capsule-core::notify` decides
 * (Notifications - Alert Classes). [wire] is the core's name: the key of the
 * `suppressed_until` / `disabled` inputs and the segment that selects a
 * `notification.*` catalog key.
 */
enum class AlertClass(
    val wire: String,
) {
    /** Two weeks without a completed sync while changes remain un-synced. */
    SYNC_STALE("sync_stale"),

    /** A recovery-secret verification check is due. */
    RECOVERY_CHECK_DUE("recovery_check_due"),

    /** Storage crossed the soft limit and is below the hard limit. */
    QUOTA_SOFT("quota_soft"),

    /** Storage is over the hard limit - the grace window is counting, or closed. */
    QUOTA_GRACE_EXPIRING("quota_grace_expiring"),

    /** Items sit on a quarantine surface awaiting a human. */
    QUARANTINE_PENDING("quarantine_pending"),

    /** Guest drops await review and adoption. */
    DROP_PENDING("drop_pending"),
    ;

    /**
     * Whether the device can compute this class's deadline alone and so pre-arm an OS
     * alarm for it. The other four are server-held and surface at next launch.
     */
    val isPreArmable: Boolean
        get() = this == SYNC_STALE || this == RECOVERY_CHECK_DUE

    companion object {
        /** The class for a wire name, or `null` for a name this build does not know. */
        fun fromWire(wire: String): AlertClass? = entries.firstOrNull { it.wire == wire }
    }
}

/** The quota state from the last `GET /v1/quota`, as the predicate reads it. */
enum class QuotaAdvisory {
    OK,
    SOFT_WARNING,
    HARD_EXCEEDED,
    GRACE_EXPIRED,
    SUSPENDED,
}

/** The parameter names the predicate emits, spelled once. */
object AlertParameter {
    const val COUNT = "count"
    const val DAYS_BEHIND = "days_behind"
    const val GRACE = "grace"
    const val SNOOZE_BUDGET = "snooze_budget"
    const val RECOVERY = "recovery"
}

/**
 * One alert true at an instant: a class and the plain values its catalog copy
 * interpolates. Never text - the words come from the app's own `notification.*`
 * resources.
 */
data class Alert(
    val alertClass: AlertClass,
    val deadline: Instant? = null,
    val params: Map<String, String> = emptyMap(),
) {
    /**
     * Whether this recovery alert is the guided re-wrap rather than the routine
     * check. The class set is closed, so this parameter is all that tells them apart,
     * and it changes where a tap goes as well as the words.
     */
    val isGuidedRewrap: Boolean
        get() = alertClass == AlertClass.RECOVERY_CHECK_DUE && params[AlertParameter.RECOVERY] == "rewrap"

    /** The catalog copy this alert renders. */
    val copy: AlertCopy
        get() = AlertCopy.of(this)

    /** Where opening it leads. */
    val destination: AlertDestination
        get() =
            when (alertClass) {
                AlertClass.SYNC_STALE -> AlertDestination.SYNC_STATUS
                AlertClass.RECOVERY_CHECK_DUE ->
                    if (isGuidedRewrap) AlertDestination.GUIDED_REWRAP else AlertDestination.RECOVERY_VERIFICATION
                AlertClass.QUOTA_SOFT, AlertClass.QUOTA_GRACE_EXPIRING -> AlertDestination.QUOTA
                AlertClass.QUARANTINE_PENDING -> AlertDestination.QUARANTINE
                AlertClass.DROP_PENDING -> AlertDestination.DROPS
            }
}

/** Where opening an alert leads, before the app turns it into a screen. */
enum class AlertDestination {
    SYNC_STATUS,
    RECOVERY_VERIFICATION,

    /** The guided re-wrap - never the routine verification prompt. */
    GUIDED_REWRAP,
    QUOTA,
    QUARANTINE,
    DROPS,
}

/**
 * The catalog keys and arguments one alert renders with.
 *
 * The `notification.*` messages are at most a whole-message plural (the only ICU
 * form the generator compiles), so an enumerated parameter (`recovery`, `grace`)
 * selects a key and a numeric one is the plural's quantity. [stem] is the catalog
 * key without its `.title` / `.body` suffix; the Android resource names are the
 * same with `.` replaced by `_`.
 */
data class AlertCopy(
    val stem: String,
    val count: Int?,
    val textArguments: List<String>,
) {
    val titleKey: String get() = "$stem.title"
    val bodyKey: String get() = "$stem.body"

    companion object {
        /** The staleness threshold in days, the fallback for a missing `days_behind`. */
        const val STALENESS_DAYS = 14

        fun of(alert: Alert): AlertCopy {
            val params = alert.params

            fun count(): Int = params[AlertParameter.COUNT]?.toIntOrNull() ?: 0
            return when (alert.alertClass) {
                AlertClass.SYNC_STALE ->
                    AlertCopy(
                        "notification.sync_stale",
                        count(),
                        listOf(params[AlertParameter.DAYS_BEHIND] ?: STALENESS_DAYS.toString()),
                    )
                AlertClass.RECOVERY_CHECK_DUE ->
                    AlertCopy(
                        if (alert.isGuidedRewrap) {
                            "notification.recovery_check_due.rewrap"
                        } else {
                            "notification.recovery_check_due.check"
                        },
                        null,
                        emptyList(),
                    )
                AlertClass.QUOTA_SOFT -> AlertCopy("notification.quota_soft", null, emptyList())
                AlertClass.QUOTA_GRACE_EXPIRING ->
                    AlertCopy(
                        if (params[AlertParameter.GRACE] == "expired") {
                            "notification.quota_grace_expiring.expired"
                        } else {
                            "notification.quota_grace_expiring.counting"
                        },
                        null,
                        emptyList(),
                    )
                AlertClass.QUARANTINE_PENDING -> AlertCopy("notification.quarantine_pending", count(), emptyList())
                AlertClass.DROP_PENDING -> AlertCopy("notification.drop_pending", count(), emptyList())
            }
        }
    }
}

/** One pre-armable class and the instant its alarm fires. */
data class ClassDeadline(
    val alertClass: AlertClass,
    val deadline: Instant,
)

/**
 * The device-held state the predicate decides from - the Kotlin mirror of the core's
 * flat `FfiNotifyInput`. An all-default value is "just installed, learned nothing".
 */
data class NotifyFacts(
    val lastCompletedSync: Instant? = null,
    val unsyncedChanges: Long = 0,
    val recoveryNextDue: Instant? = null,
    val recoverySnoozedUntil: Instant? = null,
    val recoverySnoozeBudgetSpent: Boolean = false,
    val recoveryRewrapDue: Boolean = false,
    val quotaState: QuotaAdvisory? = null,
    val quarantinePending: Long = 0,
    val dropsPending: Long = 0,
    val suppressedUntil: Map<AlertClass, Instant> = emptyMap(),
    val disabled: Set<AlertClass> = emptySet(),
) {
    /**
     * The alert an alarm armed for [deadline] presents when it fires. The core returns
     * only the class and the instant, and the alarm fires with the app not running, so
     * its parameters are fixed now from the state that produced the deadline.
     */
    fun armedAlert(deadline: ClassDeadline): Alert {
        val params =
            when (deadline.alertClass) {
                AlertClass.SYNC_STALE -> {
                    val since = lastCompletedSync ?: deadline.deadline
                    val days = maxOf(0L, deadline.deadline.epochSecond - since.epochSecond) / SECONDS_PER_DAY
                    mapOf(
                        AlertParameter.COUNT to unsyncedChanges.toString(),
                        AlertParameter.DAYS_BEHIND to days.toString(),
                    )
                }
                AlertClass.RECOVERY_CHECK_DUE ->
                    mapOf(
                        AlertParameter.SNOOZE_BUDGET to "available",
                        AlertParameter.RECOVERY to if (recoveryRewrapDue) "rewrap" else "check",
                    )
                else -> emptyMap()
            }
        return Alert(deadline.alertClass, deadline.deadline, params)
    }

    companion object {
        const val SECONDS_PER_DAY = 86_400L
    }
}
