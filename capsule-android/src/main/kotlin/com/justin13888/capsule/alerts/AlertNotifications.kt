package com.justin13888.capsule.alerts

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.os.Build
import android.os.LocaleList
import com.justin13888.capsule.MainActivity
import com.justin13888.capsule.R
import com.justin13888.capsule.i18n.AppCompatLanguageStore
import com.justin13888.capsule.i18n.AppLanguagePicker
import java.util.logging.Logger

/**
 * Posting an alert as a system notification, through `NotificationManager` directly.
 *
 * The words are resolved here, when the alarm fires, from the app's own
 * `notification.*` resources - so they follow the app language at that moment, and
 * no server can supply or influence them. Below Android 13 the in-app language reaches
 * only AppCompat activities, so the strings are resolved through [localized].
 */
object AlertNotifications {
    const val CHANNEL_ID = "capsule.alerts"

    /** The intent extra carrying a tapped alert's [AlertDestination] name. */
    const val EXTRA_DESTINATION = "com.justin13888.capsule.alerts.DESTINATION"

    private val log = Logger.getLogger("capsule.alerts")

    /**
     * Create the alerts channel, named in the app language; idempotent. Calling it again
     * after the language changes renames the existing channel.
     */
    fun ensureChannel(context: Context) {
        val words = localized(context)
        val channel =
            NotificationChannel(
                CHANNEL_ID,
                words.getString(R.string.notification_channel_name),
                NotificationManager.IMPORTANCE_DEFAULT,
            ).apply { description = words.getString(R.string.notification_channel_description) }
        context.getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    /**
     * [context], or below Android 13 with an in-app language chosen, a context configured
     * with that language, so its resources resolve in it.
     */
    fun localized(context: Context): Context {
        // Android 13+ already carries the per-app language; skip the LocaleManager round trip.
        if (!AppLanguagePicker.isNeeded(Build.VERSION.SDK_INT)) return context
        val system = context.resources.configuration.locales
        val tags =
            AlertNotificationLocale.select(
                sdkInt = Build.VERSION.SDK_INT,
                chosen = AppCompatLanguageStore.notificationLanguage(context),
                system = (0 until system.size()).map { system[it].toLanguageTag() },
            ) ?: return context
        log.fine { "notification language: locales=${tags.joinToString(",")}" }
        val configuration =
            Configuration(context.resources.configuration).apply {
                setLocales(LocaleList.forLanguageTags(tags.joinToString(",")))
            }
        return context.createConfigurationContext(configuration)
    }

    /** Whether a notification posted now would be shown. */
    fun canPost(context: Context): Boolean {
        val manager = context.getSystemService(NotificationManager::class.java)
        val granted =
            Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
                context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        return granted && manager.areNotificationsEnabled()
    }

    /**
     * Post [alert]. A refused permission is not an error: the notification is simply
     * not posted, and the class stays an in-app badge.
     */
    fun post(
        context: Context,
        alert: Alert,
    ) {
        if (!canPost(context)) return
        ensureChannel(context)
        val text = text(context, alert.copy)
        val open =
            Intent(context, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                .putExtra(EXTRA_DESTINATION, alert.destination.name)
        val tap =
            PendingIntent.getActivity(
                context,
                alert.alertClass.ordinal,
                open,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        val notification =
            Notification
                .Builder(context, CHANNEL_ID)
                .setSmallIcon(R.mipmap.ic_launcher)
                .setContentTitle(text.first)
                .setContentText(text.second)
                .setStyle(Notification.BigTextStyle().bigText(text.second))
                .setContentIntent(tap)
                .setAutoCancel(true)
                .build()
        context.getSystemService(NotificationManager::class.java).notify(alert.alertClass.ordinal, notification)
    }

    /**
     * The title and body for [copy], from the generated `notification_*` resources, in the
     * app language ([localized]).
     */
    fun text(
        context: Context,
        copy: AlertCopy,
    ): Pair<String, String> {
        val resources = localized(context).resources
        val count = copy.count ?: 0
        return when (copy.stem) {
            "notification.sync_stale" ->
                resources.getString(R.string.notification_sync_stale_title, *copy.titleArguments.toTypedArray()) to
                    resources.getQuantityString(R.plurals.notification_sync_stale_body, count, count)
            "notification.recovery_check_due.rewrap" ->
                resources.getString(R.string.notification_recovery_check_due_rewrap_title) to
                    resources.getString(R.string.notification_recovery_check_due_rewrap_body)
            "notification.recovery_check_due.check" ->
                resources.getString(R.string.notification_recovery_check_due_check_title) to
                    resources.getString(R.string.notification_recovery_check_due_check_body)
            "notification.quota_soft" ->
                resources.getString(R.string.notification_quota_soft_title) to
                    resources.getString(R.string.notification_quota_soft_body)
            "notification.quota_grace_expiring.expired" ->
                resources.getString(R.string.notification_quota_grace_expiring_expired_title) to
                    resources.getString(R.string.notification_quota_grace_expiring_expired_body)
            "notification.quota_grace_expiring.counting" ->
                resources.getString(R.string.notification_quota_grace_expiring_counting_title) to
                    resources.getString(R.string.notification_quota_grace_expiring_counting_body)
            "notification.quarantine_pending" ->
                resources.getString(R.string.notification_quarantine_pending_title) to
                    resources.getQuantityString(R.plurals.notification_quarantine_pending_body, count, count)
            else ->
                resources.getString(R.string.notification_drop_pending_title) to
                    resources.getQuantityString(R.plurals.notification_drop_pending_body, count, count)
        }
    }
}

/** Which locales a notification is rendered in. */
object AlertNotificationLocale {
    /**
     * The locale list (BCP 47 tags) to configure a notification context with, or null to
     * use the context as it is.
     *
     * Android 13+ gives every context the per-app language, so nothing is configured there.
     * Below it, a [chosen] in-app language comes first and the [system] locales follow as
     * fallbacks, without repeating the chosen one, as AppCompat orders an activity's list.
     */
    fun select(
        sdkInt: Int,
        chosen: String?,
        system: List<String>,
    ): List<String>? {
        if (!AppLanguagePicker.isNeeded(sdkInt) || chosen.isNullOrBlank()) return null
        return listOf(chosen) + system.filterNot { it.equals(chosen, ignoreCase = true) }
    }
}
