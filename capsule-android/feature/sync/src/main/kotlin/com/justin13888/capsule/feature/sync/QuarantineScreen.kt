package com.justin13888.capsule.feature.sync

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
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
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.i18n.R

/** Test handle for the list of held items. */
const val QUARANTINE_LIST_TAG: String = "sync.quarantine"

/** Every callback the quarantine surface raises. */
data class QuarantineActions(
    /** Records the user's decision about one held item. */
    val onResolve: (QuarantineItem) -> Unit,
    /** Re-reads what is held. */
    val onRetry: () -> Unit,
    /** Leaves the screen. */
    val onBack: () -> Unit,
)

/**
 * What Capsule refused to apply, and what the user can do about each.
 *
 * Every row states its reason and offers exactly one action, and there is no "apply" anywhere
 * on the screen. That is the contract the whole quarantine inventory exists to keep: inbound
 * state that failed a check is never silently dropped and never silently applied, so the only
 * way past a row is a decision a person made.
 */
@Composable
fun QuarantineScreen(
    state: QuarantineUiState,
    actions: QuarantineActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_quarantine),
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
            state.isLoading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxSize(),
                    title = stringResource(R.string.android_sync_loading),
                    contentDescription = stringResource(R.string.android_sync_loading),
                )

            state.hasFailed ->
                CapsuleErrorState(
                    title = stringResource(R.string.android_sync_quarantine_error_title),
                    modifier = Modifier.fillMaxSize(),
                    body = localizedErrorMessage(state.failure),
                    action = {
                        CapsuleButton(onClick = actions.onRetry, label = stringResource(R.string.android_sync_retry))
                    },
                )

            state.isEmpty ->
                CapsuleEmptyState(
                    title = stringResource(R.string.android_sync_quarantine_empty_title),
                    modifier = Modifier.fillMaxSize(),
                    body = stringResource(R.string.android_sync_quarantine_empty_body),
                )

            else -> HeldList(state = state, actions = actions)
        }
    }
}

@Composable
private fun HeldList(
    state: QuarantineUiState,
    actions: QuarantineActions,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize().testTag(QUARANTINE_LIST_TAG),
        verticalArrangement = Arrangement.spacedBy(RowSpacing),
        contentPadding = PaddingValues(ScreenPadding),
    ) {
        item {
            Text(
                text = stringResource(R.string.android_sync_quarantine_summary),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        items(state.items, key = QuarantineItem::key) { item ->
            HeldCard(item = item, onResolve = { actions.onResolve(item) })
        }
    }
}

@Composable
private fun HeldCard(
    item: QuarantineItem,
    onResolve: () -> Unit,
) {
    val title = quarantineTitle(item.surface)
    val reason = quarantineReason(item.surface)
    CapsuleCard(
        modifier =
            Modifier.fillMaxWidth().capsuleDescription(
                stringResource(R.string.android_sync_quarantine_item_description)
                    .withArguments("title" to title, "reason" to reason),
            ),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
            verticalArrangement = Arrangement.spacedBy(RowSpacing),
        ) {
            Text(text = title, style = MaterialTheme.typography.titleMediumEmphasized)
            Text(
                text = reason,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            item.detectedAt?.let { detected ->
                Text(
                    text =
                        stringResource(R.string.android_sync_quarantine_detected)
                            .withArguments("when" to formattedInstant(detected)),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            CapsuleBadge(
                label = stringResource(R.string.android_sync_quarantine_never_silent),
                tone = CapsuleBadgeTone.Quarantine,
            )
            CapsuleButton(
                onClick = onResolve,
                label = quarantineActionLabel(item.action),
                modifier = Modifier.fillMaxWidth(),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        }
    }
}
