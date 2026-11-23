package com.justin13888.capsule.feature.settings

import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleDivider
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.StaleSyncNotification
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy
import com.justin13888.capsule.core.model.UploadTier

/** Test handle id for the auto-sync toggle. */
internal const val AUTO_SYNC_TOGGLE = "auto-sync"

/**
 * Auto-sync, what is downloaded, the upload order, and the rules underneath both.
 *
 * The connection rules are shown and not offered. They are fixed by the protocol — an index
 * upload opens on any usable link, an original waits for an unmetered one — and presenting
 * them as settings would imply a device could opt out of a rule the whole system depends on.
 * Showing them anyway is what makes the upload-order choice above legible.
 *
 * Silencing the stale-library warning and turning off auto-sync are two separate rows for one
 * reason: a user who silences a notification has not asked to stop backing up their photos.
 */
@Composable
internal fun ColumnScope.SyncSettingsPage(
    state: SettingsUiState,
    actions: SettingsActions,
) {
    SettingsToggleRow(
        id = AUTO_SYNC_TOGGLE,
        title = stringResource(R.string.android_settings_sync_auto),
        summary = stringResource(R.string.android_settings_sync_auto_summary),
        checked = state.library.autoSyncEnabled,
        onCheckedChange = actions.onAutoSync,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsChoiceGroup(
        group = SCOPE_GROUP,
        title = stringResource(R.string.android_settings_sync_scope),
        summary = stringResource(R.string.android_settings_sync_scope_summary),
        options = SyncScope.entries.map { SettingsChoice(it, syncScopeLabel(it), it.wire) },
        selected = state.syncScope,
        onSelect = actions.onSyncScope,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsChoiceGroup(
        group = UPLOAD_GROUP,
        title = stringResource(R.string.android_settings_sync_upload_policy),
        summary = stringResource(R.string.android_settings_sync_upload_summary),
        options = UploadPolicy.entries.map { SettingsChoice(it, uploadPolicyLabel(it), it.wire) },
        selected = state.uploadPolicy,
        onSelect = actions.onUploadPolicy,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsChoiceGroup(
        group = NOTIFY_GROUP,
        title = stringResource(R.string.android_settings_sync_notifications),
        summary = stringResource(R.string.android_settings_sync_notifications_summary),
        options = staleSyncOptions(),
        selected = state.library.staleSyncNotification.asChoice(),
        onSelect = actions.onStaleSync,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    ConnectionRules(state)
}

@Composable
private fun ColumnScope.ConnectionRules(state: SettingsUiState) {
    CapsuleSectionHeader(title = stringResource(R.string.android_settings_sync_connection_rules))
    SettingsNote(stringResource(R.string.android_settings_sync_connection_rules_summary))
    UploadTier.entries.forEach { tier ->
        SettingsValueRow(
            label = uploadTierLabel(tier),
            value = connectionLabel(tier.opensAt),
        )
    }
    SettingsValueRow(
        label = stringResource(R.string.android_settings_sync_current_connection),
        value = connectionLabel(state.connection),
    )
}

@Composable
private fun staleSyncOptions(): List<SettingsChoice<StaleSyncChoice>> =
    listOf(
        SettingsChoice(
            StaleSyncChoice.Warn,
            stringResource(R.string.android_settings_sync_notify_on),
            "warn",
        ),
        SettingsChoice(
            StaleSyncChoice.Snooze,
            stringResource(R.string.android_settings_sync_notify_snoozed),
            "snooze",
        ),
        SettingsChoice(
            StaleSyncChoice.Never,
            stringResource(R.string.android_settings_sync_notify_off),
            "never",
        ),
    )

/** Which of the three offered choices a stored notification mode corresponds to. */
internal fun StaleSyncNotification.asChoice(): StaleSyncChoice =
    when (this) {
        is StaleSyncNotification.Enabled -> StaleSyncChoice.Warn
        is StaleSyncNotification.Snoozed -> StaleSyncChoice.Snooze
        is StaleSyncNotification.Disabled -> StaleSyncChoice.Never
    }

private const val SCOPE_GROUP = "sync-scope"
private const val UPLOAD_GROUP = "upload-policy"
private const val NOTIFY_GROUP = "stale-sync"
