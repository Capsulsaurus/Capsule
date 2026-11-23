package com.justin13888.capsule.feature.sync

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CloudUpload
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import java.time.Duration

/** Test handle for the sync centre's headline status. */
const val SYNC_STATUS_TAG: String = "sync.status"

/** Test handle for the user-initiated reconciliation. */
const val SYNC_FORCE_TAG: String = "sync.force"

/** Test handle for the peering section. */
const val SYNC_PEERS_TAG: String = "sync.peers"

/** Every callback the sync centre raises. */
data class SyncCentreActions(
    /** Runs a reconciliation now, ignoring the metered and Wi-Fi rules. */
    val onForceSync: () -> Unit,
    /** Suppresses the staleness warning for a window, or turns it off with `null`. */
    val onSnoozeStale: (Duration?) -> Unit,
    /** Puts the staleness warning back. */
    val onEnableStale: () -> Unit,
    /** Opens the upload and fetch queues. */
    val onOpenQueues: () -> Unit,
    /** Opens the quarantine surface. */
    val onOpenQuarantine: () -> Unit,
    /** Re-reads everything behind the screen. */
    val onRetry: () -> Unit,
    /** Leaves the screen. */
    val onBack: () -> Unit,
)

/**
 * Where the library stands relative to the server.
 *
 * Everything on this screen is a *condition* rather than an event — the link is metered, the
 * library is a fortnight behind, three items are held — which is why they are banners and rows
 * rather than transient messages: a condition that scrolls away is one the user meets again
 * without ever understanding it.
 */
@Composable
fun SyncCentreScreen(
    state: SyncCentreUiState,
    actions: SyncCentreActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_sync_centre),
            navigationIcon = {
                IconButton(onClick = actions.onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.android_shell_back),
                    )
                }
            },
        )
        when {
            state.hasFailed ->
                CapsuleErrorState(
                    title = stringResource(R.string.android_sync_error_title),
                    modifier = Modifier.fillMaxSize(),
                    body = localizedErrorMessage(state.failure),
                    action = {
                        CapsuleButton(onClick = actions.onRetry, label = stringResource(R.string.android_sync_retry))
                    },
                )

            state.isLoading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxSize(),
                    title = stringResource(R.string.android_sync_loading),
                    contentDescription = stringResource(R.string.android_sync_loading),
                )

            else -> SyncCentreBody(state = state, actions = actions)
        }
    }
}

@Composable
private fun SyncCentreBody(
    state: SyncCentreUiState,
    actions: SyncCentreActions,
) {
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        StaleNotice(state = state, actions = actions)
        if (state.failure != null) {
            SyncFailureBanner(state = state, onRetry = actions.onRetry)
        }
        StatusCard(state = state, onForceSync = actions.onForceSync)
        QueueLinks(state = state, actions = actions)
        PeeringSection(state = state)
    }
}

@Composable
private fun StatusCard(
    state: SyncCentreUiState,
    onForceSync: () -> Unit,
) {
    CapsuleCard(modifier = Modifier.fillMaxWidth().testTag(SYNC_STATUS_TAG)) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
            verticalArrangement = Arrangement.spacedBy(RowSpacing),
        ) {
            Text(text = statusHeadline(state), style = MaterialTheme.typography.headlineSmallEmphasized)
            LabelledValue(
                label = stringResource(R.string.android_sync_last_completed),
                value =
                    state.lastCompletedSync?.let { formattedInstant(it) }
                        ?: stringResource(R.string.android_sync_last_completed_never),
            )
            LabelledValue(
                label = stringResource(R.string.android_sync_connection_label),
                value = connectionLabel(state.connection),
            )
            LabelledValue(
                label = stringResource(R.string.android_sync_scope_label),
                value = scopeLabel(state.scope),
            )
            LabelledValue(
                label = stringResource(R.string.android_sync_policy_label),
                value = policyLabel(state.uploadPolicy),
            )
            Text(
                text = stringResource(R.string.android_sync_force_sync_body),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            CapsuleButton(
                onClick = onForceSync,
                label =
                    if (state.isSyncing) {
                        stringResource(R.string.android_sync_force_sync_running)
                    } else {
                        stringResource(R.string.android_sync_force_sync)
                    },
                modifier = Modifier.fillMaxWidth().testTag(SYNC_FORCE_TAG),
                emphasis = CapsuleButtonEmphasis.Primary,
                enabled = !state.isSyncing,
            )
        }
    }
}

@Composable
private fun statusHeadline(state: SyncCentreUiState): String =
    when {
        state.isOffline -> stringResource(R.string.android_sync_status_offline)
        state.lastCompletedSync == null && state.pendingUploads == 0 ->
            stringResource(R.string.android_sync_status_local_only)

        state.pendingUploads > 0 ->
            stringResource(R.string.android_sync_status_behind).withArguments("count" to state.pendingUploads)

        else -> stringResource(R.string.android_sync_status_synced)
    }

@Composable
private fun LabelledValue(
    label: String,
    value: String,
) {
    Column {
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(text = value, style = MaterialTheme.typography.bodyLarge)
    }
}

@Composable
private fun QueueLinks(
    state: SyncCentreUiState,
    actions: SyncCentreActions,
) {
    Column {
        CapsuleSectionHeader(title = stringResource(R.string.android_sync_section_queues))
        CapsuleListItem(
            headline = stringResource(R.string.android_sync_open_upload_queue),
            leading = { Icon(imageVector = Icons.Filled.CloudUpload, contentDescription = null) },
            trailing = {
                CapsuleBadge(
                    label = state.pendingUploads.toString(),
                    tone = CapsuleBadgeTone.SyncPending,
                    contentDescription =
                        stringResource(R.string.android_sync_tier_pending)
                            .withArguments("count" to state.pendingUploads),
                )
            },
            onClick = actions.onOpenQueues,
        )
        CapsuleListItem(
            headline = stringResource(R.string.android_sync_open_download_queue),
            leading = { Icon(imageVector = Icons.Filled.Download, contentDescription = null) },
            onClick = actions.onOpenQueues,
        )
        CapsuleListItem(
            headline = stringResource(R.string.android_sync_open_quarantine),
            leading = { Icon(imageVector = Icons.Filled.Shield, contentDescription = null) },
            trailing = {
                CapsuleBadge(
                    label = state.quarantined.toString(),
                    tone = CapsuleBadgeTone.Quarantine,
                    contentDescription = stringResource(R.string.android_sync_quarantine_never_silent),
                )
            },
            onClick = actions.onOpenQuarantine,
        )
    }
}

@Composable
private fun PeeringSection(state: SyncCentreUiState) {
    Column(modifier = Modifier.testTag(SYNC_PEERS_TAG)) {
        CapsuleSectionHeader(title = stringResource(R.string.android_sync_peering_title))
        Text(
            text = stringResource(R.string.android_sync_peering_body),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = ScreenPadding),
        )
        if (state.peers.isEmpty()) {
            CapsuleListItem(headline = stringResource(R.string.android_sync_peering_none))
        } else {
            state.peers.forEach { peer ->
                CapsuleListItem(
                    headline = peer.displayName ?: stringResource(R.string.android_audit_device_unknown),
                    supporting =
                        if (peer.syncedOverLocalNetwork) {
                            stringResource(R.string.android_sync_peering_synced)
                        } else {
                            stringResource(R.string.android_sync_peering_reachable)
                        },
                    overline =
                        peer.lastSeen?.let { seen ->
                            stringResource(R.string.android_sync_peering_seen)
                                .withArguments("when" to formattedInstant(seen))
                        },
                    leading = { Icon(imageVector = Icons.Filled.Devices, contentDescription = null) },
                )
            }
        }
    }
}

/** The screen's outer inset. */
internal val ScreenPadding = 16.dp

/** The gap between stacked blocks. */
internal val BlockSpacing = 16.dp

/** The gap between rows inside a card. */
internal val RowSpacing = 8.dp
