package com.justin13888.capsule.feature.sync

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleProgress
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetId

/** Test handle for the staged upload ladder. */
const val UPLOAD_QUEUE_TAG: String = "sync.upload-queue"

/** Test handle for the download and tiered-fetch queue. */
const val FETCH_QUEUE_TAG: String = "sync.fetch-queue"

/** Every callback the transfer queues raise. */
data class TransferQueueActions(
    /** Fetches one asset's missing rung now. */
    val onFetchNow: (AssetId) -> Unit,
    /** Re-derives both queues. */
    val onRetry: () -> Unit,
    /** Leaves the screen. */
    val onBack: () -> Unit,
)

/**
 * What is still owed to the server, and what is still owed to this device.
 *
 * Both queues on one screen because they are two answers to one question — "why has nothing
 * moved" — and the answer to both is usually the connection class at the top. Splitting them
 * would let a user read one half and conclude the other was broken.
 */
@Composable
fun TransferQueueScreen(
    state: TransferQueueUiState,
    actions: TransferQueueActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_upload_queue),
            subtitle = connectionLabel(state.connection),
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

            else -> QueueBody(state = state, actions = actions)
        }
    }
}

@Composable
private fun QueueBody(
    state: TransferQueueUiState,
    actions: TransferQueueActions,
) {
    LazyColumn(modifier = Modifier.fillMaxSize().testTag(UPLOAD_QUEUE_TAG)) {
        item {
            Text(
                text = stringResource(R.string.android_sync_upload_summary),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(ScreenPadding),
            )
        }
        if (state.hasNoUploads) {
            item {
                CapsuleEmptyState(
                    title = stringResource(R.string.android_sync_upload_empty_title),
                    modifier = Modifier.fillMaxWidth(),
                    body = stringResource(R.string.android_sync_upload_empty_body),
                )
            }
        }
        state.uploads.forEach { group ->
            item(key = "tier-${group.tier.wire}") { TierHeader(group) }
            items(group.entries, key = { "${group.tier.wire}-${it.id.value}" }) { entry -> UploadRow(entry) }
        }
        item { FetchHeader(state) }
        items(state.fetches, key = { "fetch-${it.id.value}" }) { entry ->
            FetchRow(entry = entry, onFetchNow = { actions.onFetchNow(entry.id) })
        }
    }
}

@Composable
private fun TierHeader(group: UploadTierGroup) {
    Column {
        CapsuleSectionHeader(
            title = tierLabel(group.tier),
            trailing = {
                Text(
                    text =
                        if (group.isClear) {
                            stringResource(R.string.android_sync_tier_clear)
                        } else {
                            stringResource(R.string.android_sync_tier_pending).withArguments("count" to group.pending)
                        },
                    style = MaterialTheme.typography.labelLarge,
                )
            },
        )
        Text(
            text = tierBody(group.tier),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = ScreenPadding),
        )
        if (!group.opensOnThisConnection && !group.isClear) {
            CapsuleBanner(
                title = stringResource(R.string.android_sync_tier_blocked),
                modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
                severity = CapsuleBannerSeverity.Degraded,
            )
        }
    }
}

@Composable
private fun UploadRow(entry: UploadQueueEntry) {
    CapsuleListItem(
        headline = formattedInstant(entry.capturedAt),
        supporting = uploadSupporting(entry),
        overline = tierLabel(entry.tier),
        trailing = { CapsuleBadge(tone = entry.progress.tone(), contentDescription = uploadSupporting(entry)) },
    )
    if (entry.progress == TierProgress.Uploading) {
        CapsuleProgress(
            progress = { entry.fraction },
            modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenPadding),
        )
    }
}

@Composable
private fun uploadSupporting(entry: UploadQueueEntry): String =
    when (entry.progress) {
        TierProgress.Uploading ->
            stringResource(R.string.android_sync_upload_progress).withArguments(
                "percent" to (entry.fraction * PERCENT).toInt(),
                "size" to formattedSize(entry.bytesTotal),
            )

        TierProgress.Failed -> localizedErrorMessage(entry.failure)
        TierProgress.Queued ->
            if (entry.pinsLocalOriginal) {
                stringResource(R.string.android_sync_upload_pinned)
            } else {
                stringResource(R.string.viewer_info_sync_awaiting_original)
            }
    }

/** The badge tone one rung's progress deserves. */
private fun TierProgress.tone(): CapsuleBadgeTone =
    when (this) {
        TierProgress.Failed -> CapsuleBadgeTone.SyncFailed
        else -> CapsuleBadgeTone.SyncPending
    }

@Composable
private fun FetchHeader(state: TransferQueueUiState) {
    Column {
        CapsuleSectionHeader(title = stringResource(R.string.android_sync_open_download_queue))
        Text(
            text = stringResource(R.string.android_sync_download_summary),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = ScreenPadding),
        )
        if (state.hasDegraded) {
            CapsuleBanner(
                title = stringResource(R.string.android_sync_degraded_title),
                modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
                body = stringResource(R.string.android_sync_degraded_body),
                severity = CapsuleBannerSeverity.Degraded,
            )
        }
        if (state.hasNoFetches) {
            CapsuleEmptyState(
                title = stringResource(R.string.android_sync_download_empty_title),
                modifier = Modifier.fillMaxWidth().testTag(FETCH_QUEUE_TAG),
                body = stringResource(R.string.android_sync_download_empty_body),
            )
        }
    }
}

@Composable
private fun FetchRow(
    entry: FetchQueueEntry,
    onFetchNow: () -> Unit,
) {
    CapsuleListItem(
        headline = formattedInstant(entry.capturedAt),
        modifier = Modifier.testTag(FETCH_QUEUE_TAG),
        supporting =
            stringResource(R.string.android_sync_download_wanted).withArguments(
                "wanted" to rungLabel(entry.wanted),
                "held" to rungLabel(entry.held),
            ),
        trailing = {
            CapsuleButton(
                onClick = onFetchNow,
                label = stringResource(R.string.android_sync_fetch_now),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        },
    )
}

/** The gap between a percentage's fraction and the number a reader expects. */
private const val PERCENT = 100
