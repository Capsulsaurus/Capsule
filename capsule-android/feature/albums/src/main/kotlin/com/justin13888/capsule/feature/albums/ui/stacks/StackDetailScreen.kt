package com.justin13888.capsule.feature.albums.ui.stacks

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ArrowDownward
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Star
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
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.AssetThumbnail
import com.justin13888.capsule.feature.albums.ui.common.detectionLabel
import com.justin13888.capsule.feature.albums.ui.common.stackTypeLabel
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * One stack, expanded into its members.
 *
 * The "best photo" control is a radio-style choice rather than a destructive one, and the
 * screen says as much by leaving every other member exactly where it was. That mirrors what
 * the operation is: a pointer move in metadata, fully reversible, never a merge.
 */
@Composable
fun StackDetailScreen(
    state: StackDetailUiState,
    actions: StackDetailActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.STACK_DETAIL)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_stack_detail),
            navigationIcon = {
                IconButton(onClick = actions.onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.common_back),
                    )
                }
            },
        )
        when (state) {
            StackDetailUiState.Loading -> CapsuleLoadingState(modifier = Modifier.fillMaxSize())
            is StackDetailUiState.Failed ->
                CapsuleErrorState(
                    title = stringResource(R.string.albums_error_load),
                    modifier = Modifier.fillMaxSize(),
                    action = {
                        CapsuleButton(onClick = actions.onRetry, label = stringResource(R.string.common_refresh))
                    },
                )

            is StackDetailUiState.Content -> StackBody(state, actions)
        }
    }
}

@Composable
private fun StackBody(
    state: StackDetailUiState.Content,
    actions: StackDetailActions,
) {
    Column(modifier = Modifier.padding(horizontal = GUTTER)) {
        Text(text = stackTypeLabel(state.stack.stackType), style = MaterialTheme.typography.titleMedium)
        detectionLabel(state.stack.detectedBy)?.let { detected ->
            Text(
                text = detected,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        itemsIndexed(state.members, key = { _, row -> row.member.id.value }) { index, row ->
            MemberRow(index = index, row = row, actions = actions)
        }
    }
}

@Composable
private fun MemberRow(
    index: Int,
    row: StackMemberRow,
    actions: StackDetailActions,
) {
    val tag = "${AlbumsTestTags.STACK_DETAIL}:${row.member.assetId.value}"
    val primaryLabel = stringResource(R.string.albums_stack_primary)
    val reorder = stringResource(R.string.albums_stack_reorder_a11y).withArgs("position" to index + 1)
    CapsuleListItem(
        headline = if (row.isPrimary) primaryLabel else row.member.memberRole.raw,
        modifier = Modifier.testTag(tag),
        supporting = reorder,
        leading = {
            Box(modifier = Modifier.size(THUMBNAIL)) {
                row.asset?.let { asset -> AssetThumbnail(asset = asset, contentDescription = primaryLabel) }
            }
        },
        trailing = {
            Row {
                if (row.isPrimary) {
                    CapsuleBadge(label = primaryLabel)
                } else {
                    IconButton(
                        onClick = { actions.onSetPrimary(row.member.assetId) },
                        modifier = Modifier.testTag("$tag:primary"),
                    ) {
                        Icon(
                            imageVector = Icons.Filled.Star,
                            contentDescription = stringResource(R.string.albums_stack_set_primary),
                        )
                    }
                }
                IconButton(
                    onClick = { actions.onMoveEarlier(index) },
                    modifier = Modifier.testTag("$tag:earlier"),
                ) {
                    Icon(
                        imageVector = Icons.Filled.ArrowUpward,
                        contentDescription = stringResource(R.string.albums_stack_move_earlier),
                    )
                }
                IconButton(
                    onClick = { actions.onMoveLater(index) },
                    modifier = Modifier.testTag("$tag:later"),
                ) {
                    Icon(
                        imageVector = Icons.Filled.ArrowDownward,
                        contentDescription = stringResource(R.string.albums_stack_move_later),
                    )
                }
            }
        },
    )
}

private val GUTTER = 16.dp
private val THUMBNAIL = 56.dp
