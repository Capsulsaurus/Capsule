package com.justin13888.capsule.feature.albums.ui.duplicates

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
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
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.albums.model.DuplicateGroup
import com.justin13888.capsule.feature.albums.model.DuplicateKind
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.AssetThumbnail
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * Duplicate candidates, grouped by how confident the finder is.
 *
 * Identical-bytes groups and near-identical ones are separated rather than merged, because
 * the decision differs: discarding a byte-identical copy is provably lossless, while a
 * near-identical frame might be the better exposure. A single list would flatten that
 * distinction into a guess.
 */
@Composable
fun DuplicatesScreen(
    state: DuplicatesUiState,
    onBack: () -> Unit,
    onKeepOnly: (DuplicateGroup, AssetId) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.DUPLICATES)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_duplicates),
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.common_back),
                    )
                }
            },
        )
        when (state) {
            DuplicatesUiState.Loading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxSize(),
                    title = stringResource(R.string.albums_duplicates_scanning),
                )

            is DuplicatesUiState.Failed ->
                CapsuleErrorState(
                    title = stringResource(R.string.albums_error_load),
                    modifier = Modifier.fillMaxSize(),
                    action = { CapsuleButton(onClick = onRetry, label = stringResource(R.string.common_refresh)) },
                )

            is DuplicatesUiState.Content -> DuplicatesBody(state, onKeepOnly)
        }
    }
}

@Composable
private fun DuplicatesBody(
    state: DuplicatesUiState.Content,
    onKeepOnly: (DuplicateGroup, AssetId) -> Unit,
) {
    if (state.groups.isEmpty()) {
        CapsuleEmptyState(
            title = stringResource(R.string.albums_duplicates_empty_title),
            body = stringResource(R.string.albums_duplicates_empty_body),
            modifier = Modifier.fillMaxSize(),
        )
        return
    }
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        if (!state.isComplete) {
            item {
                Text(
                    text = stringResource(R.string.albums_duplicates_sampled).withArgs("count" to state.sampled),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(GUTTER),
                )
            }
        }
        items(state.groups, key = { it.id }) { group -> DuplicateGroupCard(group, onKeepOnly) }
    }
}

@Composable
private fun DuplicateGroupCard(
    group: DuplicateGroup,
    onKeepOnly: (DuplicateGroup, AssetId) -> Unit,
) {
    val kind =
        stringResource(
            when (group.kind) {
                DuplicateKind.Identical -> R.string.albums_duplicates_identical
                DuplicateKind.Similar -> R.string.albums_duplicates_similar
            },
        )
    CapsuleCard(modifier = Modifier.padding(GUTTER).testTag("${AlbumsTestTags.DUPLICATES}:${group.id}")) {
        CapsuleSectionHeader(title = kind)
        Text(
            text = stringResource(R.string.albums_duplicates_group_count).withArgs("count" to group.members.size),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(GUTTER / 2)) {
            group.members.forEach { asset ->
                Box(modifier = Modifier.size(THUMBNAIL)) {
                    AssetThumbnail(asset = asset, contentDescription = kind)
                }
            }
        }
        CapsuleButton(
            onClick = { onKeepOnly(group, group.keeper.id) },
            label = stringResource(R.string.albums_duplicates_trash_others),
            emphasis = CapsuleButtonEmphasis.Destructive,
            modifier = Modifier.testTag("${AlbumsTestTags.DUPLICATES}:${group.id}:keep"),
        )
    }
}

private val GUTTER = 16.dp
private val THUMBNAIL = 72.dp
