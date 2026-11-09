package com.justin13888.capsule.alerts

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import org.json.JSONObject
import java.time.Instant

/**
 * [AlarmScheduler] over `AlarmManager` - the native API directly.
 *
 * Each class holds one `PendingIntent`, keyed by the class (its request code and its
 * data URI), so re-arming replaces rather than adds. `AlarmManager` cannot list what
 * it holds, and forgets everything at reboot, so the armed set is also kept in
 * `SharedPreferences`: [armed] reads it back, and [AlertBootReceiver] re-arms from it.
 *
 * `setAndAllowWhileIdle`, not `setExactAndAllowWhileIdle`: an alarm two weeks or ninety
 * days out does not need minute precision, an inexact alarm fires late but never early
 * (so the alert it carries is still true when it lands), and exact alarms would need
 * `SCHEDULE_EXACT_ALARM`, a special-access grant the user would have to make by hand.
 */
class AndroidAlarmScheduler(
    context: Context,
) : AlarmScheduler {
    private val context = context.applicationContext
    private val prefs = this.context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    private val alarms = this.context.getSystemService(AlarmManager::class.java)

    override fun armed(): Map<AlertClass, ArmedAlarm> =
        AlertClass.entries
            .mapNotNull { alertClass ->
                prefs.getString(alertClass.wire, null)?.let { decode(alertClass, it) }?.let { alertClass to it }
            }.toMap()

    override fun arm(alarm: ArmedAlarm) {
        schedule(alarm)
        prefs.edit().putString(alarm.alertClass.wire, encode(alarm)).apply()
    }

    override fun cancel(alertClass: AlertClass) {
        PendingIntent
            .getBroadcast(
                context,
                alertClass.ordinal,
                fireIntent(context, alertClass),
                PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE,
            )?.let {
                alarms.cancel(it)
                it.cancel()
            }
        prefs.edit().remove(alertClass.wire).apply()
    }

    /** Re-arm every persisted alarm - after a reboot cleared them from `AlarmManager`. */
    fun rearmAll() {
        armed().values.forEach(::schedule)
    }

    /** Forget a class's persisted alarm once it has fired. */
    internal fun forget(alertClass: AlertClass) {
        prefs.edit().remove(alertClass.wire).apply()
    }

    private fun schedule(alarm: ArmedAlarm) {
        val intent =
            fireIntent(context, alarm.alertClass)
                .putExtra(EXTRA_PARAMS, Bundle().apply { alarm.alert.params.forEach { (key, value) -> putString(key, value) } })
        val pending =
            PendingIntent.getBroadcast(
                context,
                alarm.alertClass.ordinal,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, alarm.fireAt.toEpochMilli(), pending)
    }

    companion object {
        private const val PREFS = "capsule.alerts.armed"
        private const val ACTION_FIRE = "com.justin13888.capsule.alerts.FIRE"
        internal const val EXTRA_CLASS = "com.justin13888.capsule.alerts.CLASS"
        internal const val EXTRA_PARAMS = "com.justin13888.capsule.alerts.PARAMS"

        /** The class-keyed intent an alarm delivers to [AlertAlarmReceiver]. */
        internal fun fireIntent(
            context: Context,
            alertClass: AlertClass,
        ): Intent =
            Intent(context, AlertAlarmReceiver::class.java)
                .setAction(ACTION_FIRE)
                .setData(Uri.parse("capsule-alert://${alertClass.wire}"))
                .putExtra(EXTRA_CLASS, alertClass.wire)

        private fun encode(alarm: ArmedAlarm): String =
            JSONObject()
                .put("fire_at", alarm.fireAt.epochSecond)
                .put("params", JSONObject(alarm.alert.params))
                .toString()

        private fun decode(
            alertClass: AlertClass,
            raw: String,
        ): ArmedAlarm? =
            runCatching {
                val json = JSONObject(raw)
                val fireAt = Instant.ofEpochSecond(json.getLong("fire_at"))
                val paramsJson = json.getJSONObject("params")
                val params = paramsJson.keys().asSequence().associateWith { paramsJson.getString(it) }
                ArmedAlarm(Alert(alertClass, fireAt, params), fireAt)
            }.getOrNull()
    }
}

/** Posts the alert an alarm carries, with the app running or not. */
class AlertAlarmReceiver : BroadcastReceiver() {
    override fun onReceive(
        context: Context,
        intent: Intent,
    ) {
        val alertClass = intent.getStringExtra(AndroidAlarmScheduler.EXTRA_CLASS)?.let(AlertClass::fromWire) ?: return
        val bundle = intent.getBundleExtra(AndroidAlarmScheduler.EXTRA_PARAMS)
        val params = bundle?.keySet()?.associateWith { bundle.getString(it).orEmpty() } ?: emptyMap()
        AndroidAlarmScheduler(context).forget(alertClass)
        AlertNotifications.post(context, Alert(alertClass, params = params))
    }
}

/** Re-arms the persisted alarms that a reboot cleared from `AlarmManager`. */
class AlertBootReceiver : BroadcastReceiver() {
    override fun onReceive(
        context: Context,
        intent: Intent,
    ) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        AndroidAlarmScheduler(context).rearmAll()
    }
}
