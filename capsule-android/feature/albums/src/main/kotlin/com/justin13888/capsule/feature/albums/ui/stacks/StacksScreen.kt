package com.justin13888.capsule.feature.albums.ui.stacks

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.AssetThumbnail
import com.justin13888.capsule.feature.albums.ui.common.stackTypeLabel
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/** Every group of related files, largest first. */
@Composable
fun StacksScreen(
    state: StacksUiState,
    onBack: () -> Unit,
    onOpen: (StackId) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.STACKS)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.albums_utility_stacks),
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
            StacksUiState.Loading -> CapsuleLoadingState(modifier = Modifier.fillMaxSize())
            is StacksUiState.Failed ->
                CapsuleErrorState(
                    title = stringResource(R.string.albums_error_load),
                    modifier = Modifier.fillMaxSize(),
                    action = { CapsuleButton(onClick = onRetry, label = stringResource(R.string.common_refresh)) },
                )

            is StacksUiState.Content ->
                if (state.rows.isEmpty()) {
                    CapsuleEmptyState(
                        title = stringResource(R.string.albums_stacks_empty_title),
                        body = stringResource(R.string.albums_stacks_empty_body),
                        modifier = Modifier.fillMaxSize(),
                    )
                } else {
                    LazyColumn(modifier = Modifier.fillMaxSize()) {
                        items(state.rows, key = { it.id.value }) { row -> StackRowItem(row, onOpen) }
                    }
                }
        }
    }
}

@Composable
private fun StackRowItem(
    row: StackRow,
    onOpen: (StackId) -> Unit,
) {
    val type = stackTypeLabel(row.stackType)
    CapsuleListItem(
        headline = type,
        modifier = Modifier.testTag("${AlbumsTestTags.STACKS}:${row.id.value}"),
        supporting = stringResource(R.string.albums_stack_member_count).withArgs("count" to row.memberCount),
        leading = {
            Box(modifier = Modifier.size(THUMBNAIL)) {
                AssetThumbnail(asset = row.cover, contentDescription = type)
            }
        },
        onClick = { onOpen(row.id) },
    )
}

private val THUMBNAIL = 56.dp
