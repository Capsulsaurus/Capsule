package com.justin13888.capsule.feature.albums.ui.detail

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Group
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonGroup
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonGroupItem
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.albums.ui.common.AlbumNameDialog
import com.justin13888.capsule.feature.albums.ui.common.AlbumsGallery
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.ConfirmDialog
import com.justin13888.capsule.feature.albums.ui.common.systemViewLabel
import com.justin13888.capsule.feature.albums.ui.common.withArgs
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList

/** One album's contents, plus the sheets that explain and change what it is. */
@Composable
fun AlbumDetailScreen(
    state: AlbumDetailUiState,
    actions: AlbumDetailActions,
    modifier: Modifier = Modifier,
) {
    when (state) {
        AlbumDetailUiState.Loading ->
            CapsuleLoadingState(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.DETAIL))

        is AlbumDetailUiState.Failed ->
            CapsuleErrorState(
                title = stringResource(R.string.albums_error_load),
                modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.DETAIL),
                action = { RetryButton(actions.onRetry) },
            )

        is AlbumDetailUiState.Content -> DetailContent(state, actions, modifier)
    }
}

@Composable
private fun RetryButton(onRetry: () -> Unit) {
    CapsuleButtonGroup(
        items = persistentListOf(CapsuleButtonGroupItem(stringResource(R.string.common_refresh), onRetry)),
    )
}

@Composable
private fun DetailContent(
    state: AlbumDetailUiState.Content,
    actions: AlbumDetailActions,
    modifier: Modifier,
) {
    var sheet by remember { mutableStateOf(DetailSheet.None) }
    var renaming by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }

    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.DETAIL)) {
        CapsuleTopAppBar(
            title = resolveTitle(state),
            navigationIcon = {
                IconButton(onClick = actions.onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.common_back),
                    )
                }
            },
        )
        DetailActions(
            state = state,
            onOpenSheet = { sheet = it },
            onRename = { renaming = true },
            onDelete = { deleting = true },
            onMakeDefault = actions.onMakeDefault,
        )
        state.message?.let { message -> DetailMessage(message, actions.onDismissMessage) }
        state.federated?.let { group -> FederatedBanners(group) }
        if (state.isTruncated) TruncationNote(state.assets.size)
        Contents(state, actions.onOpenAsset)
    }

    when (sheet) {
        DetailSheet.None -> Unit
        DetailSheet.Members ->
            AlbumMembersSheet(state = state, actions = actions, onDismiss = { sheet = DetailSheet.None })

        DetailSheet.Policy ->
            AlbumPolicySheet(state = state, actions = actions, onDismiss = { sheet = DetailSheet.None })
    }
    if (renaming) {
        AlbumNameDialog(
            title = stringResource(R.string.albums_rename_title),
            initialName = state.album?.name.orEmpty(),
            onConfirm = {
                renaming = false
                actions.onRename(it)
            },
            onDismiss = { renaming = false },
            testTag = "albums.detail.rename",
        )
    }
    if (deleting) {
        ConfirmDialog(
            title = stringResource(R.string.albums_delete_confirm_title),
            body = stringResource(R.string.albums_delete_confirm_body),
            confirmLabel = stringResource(R.string.common_delete),
            onConfirm = {
                deleting = false
                actions.onDelete()
            },
            onDismiss = { deleting = false },
            testTag = "albums.detail.delete",
        )
    }
}

/** Which sheet, if any, is over the contents. */
private enum class DetailSheet { None, Members, Policy }

@Composable
private fun ColumnScope.DetailActions(
    state: AlbumDetailUiState.Content,
    onOpenSheet: (DetailSheet) -> Unit,
    onRename: () -> Unit,
    onDelete: () -> Unit,
    onMakeDefault: () -> Unit,
) {
    if (state.kind !is AlbumDetailKind.Container) return
    val items =
        buildList {
            add(
                CapsuleButtonGroupItem(
                    label = stringResource(R.string.albums_detail_members),
                    onClick = { onOpenSheet(DetailSheet.Members) },
                    icon = Icons.Filled.Group,
                ),
            )
            add(
                CapsuleButtonGroupItem(
                    label = stringResource(R.string.albums_detail_policy),
                    onClick = { onOpenSheet(DetailSheet.Policy) },
                    icon = Icons.Filled.Shield,
                ),
            )
            add(
                CapsuleButtonGroupItem(
                    label = stringResource(R.string.albums_action_rename),
                    onClick = onRename,
                    icon = Icons.Filled.Edit,
                    enabled = state.isAdmin,
                ),
            )
            if (!state.isDefault) {
                add(
                    CapsuleButtonGroupItem(
                        label = stringResource(R.string.albums_action_set_default),
                        onClick = onMakeDefault,
                        icon = Icons.Filled.Star,
                    ),
                )
            }
            add(
                CapsuleButtonGroupItem(
                    label = stringResource(R.string.albums_action_delete),
                    onClick = onDelete,
                    icon = Icons.Filled.Delete,
                    enabled = state.isAdmin && !state.isDefault,
                ),
            )
        }
    CapsuleButtonGroup(
        items = items.toImmutableList(),
        modifier = Modifier.padding(horizontal = GUTTER),
    )
}

@Composable
private fun DetailMessage(
    message: AlbumDetailMessage,
    onDismiss: () -> Unit,
) {
    CapsuleBanner(
        title = detailMessageText(message),
        modifier = Modifier.padding(GUTTER).testTag("${AlbumsTestTags.DETAIL}:message"),
        severity = CapsuleBannerSeverity.Warning,
        action = {
            CapsuleButtonGroup(
                items = persistentListOf(CapsuleButtonGroupItem(stringResource(R.string.common_close), onDismiss)),
            )
        },
    )
}

@Composable
private fun TruncationNote(loaded: Int) {
    Text(
        text = stringResource(R.string.albums_detail_showing_first).withArgs("count" to loaded),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = GUTTER, vertical = GUTTER / 2),
    )
}

@Composable
private fun ColumnScope.Contents(
    state: AlbumDetailUiState.Content,
    onOpenAsset: (AssetId) -> Unit,
) {
    if (state.assets.isEmpty()) {
        CapsuleEmptyState(
            title = stringResource(R.string.albums_detail_empty_title),
            body = stringResource(R.string.albums_detail_empty_body),
            modifier = Modifier.weight(1f),
        )
        return
    }
    val cover = stringResource(R.string.albums_a11y_cover)
    AlbumsGallery(
        assets = state.assets,
        contentDescription = resolveTitle(state),
        cellDescription = { cover },
        modifier = Modifier.weight(1f),
        onOpen = onOpenAsset,
    )
}

/** The album's name, with the nameless default and the built-in views given theirs. */
@Composable
private fun resolveTitle(state: AlbumDetailUiState.Content): String {
    val fallback = stringResource(R.string.albums_default_album)
    return when (val kind = state.kind) {
        AlbumDetailKind.Container -> state.title.ifEmpty { fallback }
        is AlbumDetailKind.View -> systemViewLabel(kind.kind)
        AlbumDetailKind.Aggregate -> state.title.ifEmpty { fallback }
    }
}

/** The gutter the app's list content sits in. */
internal val GUTTER = 16.dp
