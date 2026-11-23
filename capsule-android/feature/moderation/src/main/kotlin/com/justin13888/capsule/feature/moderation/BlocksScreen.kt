package com.justin13888.capsule.feature.moderation

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.UserBlock

/** Test handle for the whole blocked-list surface. */
const val MODERATION_BLOCKS_TAG: String = "moderation.blocks"

/** Test handle for the add field. */
const val MODERATION_BLOCKS_FIELD_TAG: String = "moderation.blocks.field"

/** Test handle for the loading state. */
const val MODERATION_BLOCKS_LOADING_TAG: String = "moderation.blocks.loading"

/** Test handle for the empty state. */
const val MODERATION_BLOCKS_EMPTY_TAG: String = "moderation.blocks.empty"

/** Test handle for the error state. */
const val MODERATION_BLOCKS_ERROR_TAG: String = "moderation.blocks.error"

/** Test handle for one blocked row: `moderation.block:<id>`. */
fun moderationBlockTag(accountId: String): String = "moderation.block:$accountId"

/**
 * The accounts this user has blocked, and the one place to add another.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. The scope note is permanent rather than a first-run tip: a user who believes a
 * block severs a whole server will over-use it, and a user who believes it is invisible will
 * under-use it — both are corrected by saying, every time, exactly how far it reaches.
 *
 * @param state everything to draw.
 * @param onDraftChange edits the account to block.
 * @param onBlock places the block.
 * @param onUnblock lifts one.
 * @param onRetry re-reads the list after a failure.
 */
@Composable
fun BlocksScreen(
    state: BlocksUiState,
    onDraftChange: (String) -> Unit,
    onBlock: () -> Unit,
    onUnblock: (String) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(MODERATION_BLOCKS_TAG)) {
        CapsuleTopAppBar(title = stringResource(R.string.android_destination_moderation_blocks))
        Column(
            modifier = Modifier.fillMaxWidth().padding(ScreenInset),
            verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
        ) {
            Text(
                text = stringResource(R.string.android_moderation_blocks_body),
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                text = stringResource(R.string.android_moderation_blocks_epoch_note),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            BlockAddRow(state, onDraftChange, onBlock)
        }
        when {
            state.isLoading -> BlocksLoadingState()
            state.failure != null && state.blocks.isEmpty() -> BlocksFailureState(state.failure, onRetry)
            state.isEmpty -> BlocksEmptyState()
            else -> BlockList(state, onUnblock)
        }
    }
}

@Composable
private fun BlockAddRow(
    state: BlocksUiState,
    onDraftChange: (String) -> Unit,
    onBlock: () -> Unit,
) {
    OutlinedTextField(
        value = state.draft,
        onValueChange = onDraftChange,
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_BLOCKS_FIELD_TAG),
        label = { Text(text = stringResource(R.string.android_moderation_blocks_add_label)) },
        supportingText = {
            if (state.draftMissing) Text(text = stringResource(R.string.android_moderation_blocks_id_required))
        },
        isError = state.draftMissing,
        singleLine = true,
        colors = capsuleFieldColors(),
    )
    CapsuleButton(
        onClick = onBlock,
        label = stringResource(R.string.android_moderation_blocks_add),
        enabled = state.canAdd,
    )
}

@Composable
private fun BlocksLoadingState() {
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_BLOCKS_LOADING_TAG),
        title = stringResource(R.string.android_moderation_blocks_loading),
        contentDescription = stringResource(R.string.android_moderation_blocks_loading),
    )
}

@Composable
private fun BlocksEmptyState() {
    CapsuleEmptyState(
        title = stringResource(R.string.android_moderation_blocks_empty_title),
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_BLOCKS_EMPTY_TAG),
        body = stringResource(R.string.android_moderation_blocks_empty_body),
    )
}

@Composable
private fun BlocksFailureState(
    failure: ModerationFailure,
    onRetry: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_moderation_blocks_error_title),
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_BLOCKS_ERROR_TAG),
        body = localizedErrorMessage(failure.code),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_moderation_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun BlockList(
    state: BlocksUiState,
    onUnblock: (String) -> Unit,
) {
    val listDescription = stringResource(R.string.android_moderation_blocks_list_description)
    LazyColumn(modifier = Modifier.fillMaxSize().capsuleDescription(listDescription)) {
        items(items = state.blocks, key = { it.blockedUserId.value }) { block ->
            BlockRow(block = block, pending = state.isPending(block.blockedUserId.value), onUnblock = onUnblock)
        }
    }
}

/**
 * One blocked account.
 *
 * The unblock button carries its own spoken description naming the account: a row of buttons
 * that all announce "Unblock" is a list a screen reader user cannot act on.
 */
@Composable
private fun BlockRow(
    block: UserBlock,
    pending: Boolean,
    onUnblock: (String) -> Unit,
) {
    val id = block.blockedUserId.value
    val blockedAt =
        stringResource(R.string.android_moderation_blocks_blocked_at)
            .withArguments("date" to formattedInstant(block.createdAt))
    CapsuleListItem(
        headline = id,
        modifier = Modifier.testTag(moderationBlockTag(id)),
        supporting = blockedAt,
        trailing = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                CapsuleButton(
                    onClick = { onUnblock(id) },
                    label = stringResource(R.string.android_moderation_blocks_unblock),
                    emphasis = CapsuleButtonEmphasis.Secondary,
                    enabled = !pending,
                    contentDescription =
                        stringResource(R.string.android_moderation_blocks_unblock_description)
                            .withArguments("name" to id),
                )
            }
        },
        contentDescription = id + SEPARATOR + blockedAt,
    )
}
