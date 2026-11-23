package com.justin13888.capsule.feature.albums.ui.trash

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.DeleteForever
import androidx.compose.material.icons.filled.Restore
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.AssetThumbnail
import com.justin13888.capsule.feature.albums.ui.common.ConfirmDialog
import com.justin13888.capsule.feature.albums.ui.common.asIsoDate
import com.justin13888.capsule.feature.albums.ui.common.retentionLabel
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * Recently Deleted: what is recoverable, for how much longer, and what may now be purged.
 *
 * A list rather than a grid, because the load-bearing information on this screen is not the
 * photograph — it is the countdown beside it. A user opens this view to answer "have I still
 * got time", and a justified grid of thumbnails answers a different question.
 */
@Composable
fun RecentlyDeletedScreen(
    state: RecentlyDeletedUiState,
    onBack: () -> Unit,
    onRestore: (AssetId) -> Unit,
    onEmptyTrash: () -> Unit,
    onDismissRefusal: () -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var confirming by remember { mutableStateOf(false) }
    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.TRASH)) {
        TrashBar(
            canEmpty = (state as? RecentlyDeletedUiState.Content)?.hasPurgeable == true,
            onBack = onBack,
            onEmpty = { confirming = true },
        )
        when (state) {
            RecentlyDeletedUiState.Loading -> CapsuleLoadingState(modifier = Modifier.fillMaxSize())
            is RecentlyDeletedUiState.Failed ->
                CapsuleErrorState(
                    title = stringResource(R.string.albums_error_load),
                    modifier = Modifier.fillMaxSize(),
                    action = { CapsuleButton(onClick = onRetry, label = stringResource(R.string.common_refresh)) },
                )

            is RecentlyDeletedUiState.Content -> TrashList(state, onRestore, onDismissRefusal)
        }
    }
    if (confirming) {
        ConfirmDialog(
            title = stringResource(R.string.albums_trash_empty_confirm_title),
            body = stringResource(R.string.albums_trash_empty_confirm_body),
            confirmLabel = stringResource(R.string.library_trash_empty_button),
            onConfirm = {
                confirming = false
                onEmptyTrash()
            },
            onDismiss = { confirming = false },
            testTag = "albums.trash.empty",
        )
    }
}

@Composable
private fun TrashBar(
    canEmpty: Boolean,
    onBack: () -> Unit,
    onEmpty: () -> Unit,
) {
    CapsuleTopAppBar(
        title = stringResource(R.string.nav_trash),
        navigationIcon = {
            IconButton(onClick = onBack) {
                Icon(
                    imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = stringResource(R.string.common_back),
                )
            }
        },
        actions = {
            IconButton(
                onClick = onEmpty,
                enabled = canEmpty,
                modifier = Modifier.testTag("${AlbumsTestTags.TRASH}:empty"),
            ) {
                Icon(
                    imageVector = Icons.Filled.DeleteForever,
                    contentDescription = stringResource(R.string.library_trash_empty_button),
                )
            }
        },
    )
}

@Composable
private fun TrashList(
    state: RecentlyDeletedUiState.Content,
    onRestore: (AssetId) -> Unit,
    onDismissRefusal: () -> Unit,
) {
    if (state.refusedPurge) {
        CapsuleBanner(
            title = stringResource(R.string.albums_trash_purge_refused),
            modifier = Modifier.padding(GUTTER).testTag("${AlbumsTestTags.TRASH}:refused"),
            severity = CapsuleBannerSeverity.Warning,
            action = { CapsuleButton(onClick = onDismissRefusal, label = stringResource(R.string.common_close)) },
        )
    }
    if (state.items.isEmpty()) {
        CapsuleEmptyState(
            title = stringResource(R.string.library_trash_empty),
            body = stringResource(R.string.albums_trash_empty_body),
            modifier = Modifier.fillMaxSize(),
        )
        return
    }
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        items(state.items, key = { it.asset.id.value }) { item -> TrashRow(item, onRestore) }
    }
}

@Composable
private fun TrashRow(
    item: TrashedItem,
    onRestore: (AssetId) -> Unit,
) {
    val remaining = retentionLabel(item.countdown)
    val announced = stringResource(R.string.albums_trash_item_a11y).withArgs("remaining" to remaining)
    val restore = stringResource(R.string.albums_trash_restore)
    CapsuleListItem(
        headline = remaining,
        modifier = Modifier.testTag("${AlbumsTestTags.TRASH_ITEM}:${item.asset.id.value}"),
        supporting = item.deletedAt.asIsoDate(),
        leading = {
            Box(modifier = Modifier.size(THUMBNAIL)) {
                AssetThumbnail(asset = item.asset, contentDescription = announced)
            }
        },
        trailing = {
            if (item.isPurgeable) {
                CapsuleBadge(label = remaining, tone = CapsuleBadgeTone.Degraded)
            } else {
                IconButton(
                    onClick = { onRestore(item.asset.id) },
                    modifier = Modifier.testTag("${AlbumsTestTags.TRASH_ITEM}:${item.asset.id.value}:restore"),
                ) { Icon(Icons.Filled.Restore, contentDescription = restore) }
            }
        },
        contentDescription = announced,
    )
}

private val GUTTER = 16.dp
private val THUMBNAIL = 56.dp
