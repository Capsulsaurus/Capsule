package com.justin13888.capsule.alerts

import android.Manifest
import android.content.Context
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.os.Build
import org.json.JSONObject
import java.time.Instant

/**
 * The client's own alert facts, persisted so the predicate can read them on any launch:
 * the last completed sync and the un-synced count, the last quota state, the quarantine
 * and pending-drop counts, the recovery cadence's projection, and the user's disables.
 *
 * Each `record*` is called by the flow that learns the fact (a completed sync, a quota
 * read, a quarantine or drop listing, the recovery scheduler, the settings screen).
 */
class SharedPreferencesAlertFacts(
    context: Context,
) : AlertFactsSource {
    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences("capsule.alerts.facts", Context.MODE_PRIVATE)

    override fun facts(): NotifyFacts =
        NotifyFacts(
            lastCompletedSync = instant(LAST_SYNC),
            unsyncedChanges = prefs.getLong(UNSYNCED, 0),
            recoveryNextDue = instant(RECOVERY_DUE),
            recoverySnoozedUntil = instant(RECOVERY_SNOOZED),
            recoverySnoozeBudgetSpent = prefs.getBoolean(RECOVERY_BUDGET_SPENT, false),
            recoveryRewrapDue = prefs.getBoolean(RECOVERY_REWRAP, false),
            quotaState = prefs.getString(QUOTA, null)?.let { name -> QuotaAdvisory.entries.firstOrNull { it.name == name } },
            quarantinePending = prefs.getLong(QUARANTINE, 0),
            dropsPending = prefs.getLong(DROPS, 0),
            disabled =
                prefs
                    .getStringSet(DISABLED, emptySet())
                    .orEmpty()
                    .mapNotNull(AlertClass::fromWire)
                    .toSet(),
        )

    /** A sync completed at [at], leaving [unsynced] changes still to send. */
    fun recordCompletedSync(
        at: Instant,
        unsynced: Long,
    ) = prefs
        .edit()
        .putLong(LAST_SYNC, at.epochSecond)
        .putLong(UNSYNCED, unsynced)
        .apply()

    /** The un-synced count changed between syncs. */
    fun recordUnsynced(unsynced: Long) = prefs.edit().putLong(UNSYNCED, unsynced).apply()

    /** The state from the latest `GET /v1/quota`. */
    fun recordQuota(state: QuotaAdvisory) = prefs.edit().putString(QUOTA, state.name).apply()

    /** Items on the quarantine surfaces, **excluding** pending drops. */
    fun recordQuarantinePending(count: Long) = prefs.edit().putLong(QUARANTINE, count).apply()

    /** Guest drops awaiting review. */
    fun recordDropsPending(count: Long) = prefs.edit().putLong(DROPS, count).apply()

    /** The recovery cadence's projection (`RecoveryCadence::notify_facts`); `null` before setup. */
    fun recordRecovery(
        nextDue: Instant?,
        snoozedUntil: Instant?,
        budgetSpent: Boolean,
        rewrapDue: Boolean,
    ) = prefs
        .edit()
        .putInstant(RECOVERY_DUE, nextDue)
        .putInstant(RECOVERY_SNOOZED, snoozedUntil)
        .putBoolean(RECOVERY_BUDGET_SPENT, budgetSpent)
        .putBoolean(RECOVERY_REWRAP, rewrapDue)
        .apply()

    /** Turn a class's warning on or off. Suppresses the warning only, never the behavior. */
    fun setDisabled(
        alertClass: AlertClass,
        disabled: Boolean,
    ) {
        val current = prefs.getStringSet(DISABLED, emptySet()).orEmpty().toMutableSet()
        if (disabled) current += alertClass.wire else current -= alertClass.wire
        prefs.edit().putStringSet(DISABLED, current).apply()
    }

    private fun instant(key: String): Instant? = if (prefs.contains(key)) Instant.ofEpochSecond(prefs.getLong(key, 0)) else null

    private fun SharedPreferences.Editor.putInstant(
        key: String,
        value: Instant?,
    ): SharedPreferences.Editor = if (value == null) remove(key) else putLong(key, value.epochSecond)

    private companion object {
        const val LAST_SYNC = "last_completed_sync"
        const val UNSYNCED = "unsynced_changes"
        const val RECOVERY_DUE = "recovery_next_due"
        const val RECOVERY_SNOOZED = "recovery_snoozed_until"
        const val RECOVERY_BUDGET_SPENT = "recovery_snooze_budget_spent"
        const val RECOVERY_REWRAP = "recovery_rewrap_due"
        const val QUOTA = "quota_state"
        const val QUARANTINE = "quarantine_pending"
        const val DROPS = "drops_pending"
        const val DISABLED = "disabled"
    }
}

/** The snooze ledger in `SharedPreferences`; a decode failure starts from empty. */
class SharedPreferencesAlertSnoozeStore(
    context: Context,
) : AlertSnoozeStore {
    private val prefs = context.applicationContext.getSharedPreferences("capsule.alerts.snoozes", Context.MODE_PRIVATE)

    override fun load(): AlertSnoozeLedger {
        val entries =
            AlertClass.entries
                .mapNotNull { alertClass ->
                    prefs.getString(alertClass.wire, null)?.let { raw ->
                        runCatching {
                            val json = JSONObject(raw)
                            alertClass to AlertSnoozeLedger.Entry(Instant.ofEpochSecond(json.getLong("until")), json.getInt("count"))
                        }.getOrNull()
                    }
                }.toMap()
        return AlertSnoozeLedger(entries)
    }

    override fun save(ledger: AlertSnoozeLedger) {
        val editor = prefs.edit().clear()
        ledger.entries.forEach { (alertClass, entry) ->
            editor.putString(
                alertClass.wire,
                JSONObject().put("until", entry.until.epochSecond).put("count", entry.count).toString(),
            )
        }
        editor.apply()
    }
}

/**
 * [NotificationPermission] over `POST_NOTIFICATIONS` (API 33+).
 *
 * Android reports "denied" both before and after the prompt, so whether it was asked is
 * remembered here: denied after asking is a refusal, never re-prompted. Below API 33
 * the grant is implicit, and only the user's own notification switch can refuse it.
 * The prompt itself needs an activity: [launcher] is set by the visible activity, and
 * a request with none visible waits for the next refresh that has one.
 */
class AndroidNotificationPermission(
    context: Context,
) : NotificationPermission {
    private val context = context.applicationContext
    private val prefs = this.context.getSharedPreferences("capsule.alerts.permission", Context.MODE_PRIVATE)

    /** Starts the system prompt; set by the visible activity, cleared when it goes. */
    @Volatile
    var launcher: (() -> Unit)? = null

    override fun state(): NotificationPermissionState =
        when {
            AlertNotifications.canPost(context) -> NotificationPermissionState.GRANTED
            Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU -> NotificationPermissionState.DENIED
            context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED ->
                NotificationPermissionState.DENIED
            prefs.getBoolean(ASKED, false) -> NotificationPermissionState.DENIED
            else -> NotificationPermissionState.NOT_DETERMINED
        }

    override fun request() {
        val launch = launcher ?: return
        prefs.edit().putBoolean(ASKED, true).apply()
        launch()
    }

    private companion object {
        const val ASKED = "asked"
    }
}
