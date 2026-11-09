package com.justin13888.capsule.alerts

import android.content.Context

/**
 * The alert loop's composition for the Android app: the core's predicate over FFI,
 * `AlarmManager`, `POST_NOTIFICATIONS`, and the persisted facts and snoozes.
 */
class AlertsGraph(
    context: Context,
) {
    val facts = SharedPreferencesAlertFacts(context)
    val permission = AndroidNotificationPermission(context)
    val coordinator =
        AlertCoordinator(
            facts = facts,
            policy = FfiAlertPolicy(),
            scheduler = AndroidAlarmScheduler(context),
            permission = permission,
            snoozes = SharedPreferencesAlertSnoozeStore(context),
        )
}
