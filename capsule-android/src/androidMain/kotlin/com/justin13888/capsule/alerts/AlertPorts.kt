package com.justin13888.capsule.alerts

import java.time.Instant

/**
 * The alert predicate - `capsule-core::notify`, one decision function for every
 * platform. The app adapts the core's exports ([FfiAlertPolicy]); tests script it.
 */
interface AlertPolicy {
    /** Every alert true at [now] - the in-app badge set. */
    fun evaluate(
        facts: NotifyFacts,
        now: Instant,
    ): List<Alert>

    /**
     * The instant to arm an alarm for, **per class**: a class present holds exactly
     * one alarm at that instant, a class absent holds none. Never collapse it to the
     * earliest deadline - that loses the later class's alarm on a device the app is
     * not opened on again.
     */
    fun preArmDeadlines(
        facts: NotifyFacts,
        now: Instant,
    ): List<ClassDeadline>
}

/** The device-held state the predicate reads. */
fun interface AlertFactsSource {
    fun facts(): NotifyFacts
}

/** One alarm armed with the OS for a class, with the copy it fires with. */
data class ArmedAlarm(
    val alert: Alert,
    val fireAt: Instant,
) {
    val alertClass: AlertClass get() = alert.alertClass
}

/**
 * The OS alarm scheduler, keyed by class: it holds at most one alarm per class, and
 * arming a class that holds one replaces it.
 */
interface AlarmScheduler {
    fun armed(): Map<AlertClass, ArmedAlarm>

    fun arm(alarm: ArmedAlarm)

    fun cancel(alertClass: AlertClass)
}

/** Whether the user lets the app post notifications. */
enum class NotificationPermissionState {
    /** Never asked. */
    NOT_DETERMINED,

    /** Allowed. */
    GRANTED,

    /** Refused - not an error: every class degrades to its in-app badge. */
    DENIED,
}

/**
 * The notification-permission prompt, requested the first time a class has something
 * to arm and never at launch. Android answers asynchronously, so [request] only
 * starts the prompt; its answer arrives through a later refresh.
 */
interface NotificationPermission {
    fun state(): NotificationPermissionState

    fun request()
}

/** Where the snooze ledger persists between launches. */
interface AlertSnoozeStore {
    fun load(): AlertSnoozeLedger

    fun save(ledger: AlertSnoozeLedger)
}
