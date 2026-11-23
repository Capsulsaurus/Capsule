package com.justin13888.capsule.feature.albums.ui.hub

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Icon
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
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.albums.model.CollectionRef
import com.justin13888.capsule.feature.albums.model.FederatedAlbum
import com.justin13888.capsule.feature.albums.ui.common.AlbumNameDialog
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.systemViewLabel
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * Everything the user can reach that is not the timeline, in one screen.
 *
 * Stateless, so every one of its states — loading, failed, first-run empty, and a populated
 * account — is reachable from a preview or a test without a view model, a repository, or a
 * coroutine.
 *
 * @param onOpen opens an album, a built-in view, the stacks listing, or an aggregate.
 * @param onOpenGated opens Recently Deleted or Hidden. Separate from [onOpen] because those
 *   two carry a local-auth gate applied by the routing table: they have to be *pushed*, and
 *   a caller that opened one in a pane would render it without ever passing through its
 *   entry, and so without the gate.
 */
@Composable
@Suppress("LongParameterList")
fun CollectionsHubScreen(
    state: CollectionsHubUiState,
    onOpen: (CollectionRef) -> Unit,
    onOpenSmartAlbums: () -> Unit,
    onOpenDuplicates: () -> Unit,
    onOpenPlaces: () -> Unit,
    onOpenGated: (GatedSection) -> Unit,
    onCreateAlbum: (String) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var creating by remember { mutableStateOf(false) }
    when (state) {
        CollectionsHubUiState.Loading ->
            CapsuleLoadingState(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.HUB))

        is CollectionsHubUiState.Failed ->
            CapsuleErrorState(
                title = stringResource(R.string.albums_error_load),
                modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.HUB),
                action = {
                    CapsuleButton(onClick = onRetry, label = stringResource(R.string.common_refresh))
                },
            )

        is CollectionsHubUiState.Content -> {
            HubBody(
                state = state,
                onOpen = onOpen,
                onOpenSmartAlbums = onOpenSmartAlbums,
                onOpenDuplicates = onOpenDuplicates,
                onOpenPlaces = onOpenPlaces,
                onOpenGated = onOpenGated,
                onCreateAlbum = { creating = true },
                modifier = modifier,
            )
            if (creating) {
                AlbumNameDialog(
                    title = stringResource(R.string.albums_create_title),
                    initialName = "",
                    onConfirm = {
                        creating = false
                        onCreateAlbum(it)
                    },
                    onDismiss = { creating = false },
                    testTag = "albums.hub.create",
                )
            }
        }
    }
}

/** The two sections that must be pushed rather than shown in place, because they are gated. */
enum class GatedSection {
    /** Recently Deleted. */
    Trash,

    /** The Hidden view. */
    Hidden,
}

@Composable
@Suppress("LongParameterList")
private fun HubBody(
    state: CollectionsHubUiState.Content,
    onOpen: (CollectionRef) -> Unit,
    onOpenSmartAlbums: () -> Unit,
    onOpenDuplicates: () -> Unit,
    onOpenPlaces: () -> Unit,
    onOpenGated: (GatedSection) -> Unit,
    onCreateAlbum: () -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.HUB)) {
        item { MyAlbumsHeader(onCreateAlbum) }
        if (state.isEmpty) {
            item { EmptyHub(onCreateAlbum) }
        }
        items(state.myAlbums, key = { it.id.value }) { album -> AlbumRow(album, onOpen) }

        item { SectionHeader(stringResource(R.string.albums_section_media_types), "media-types") }
        items(state.mediaTypes, key = { it.kind.token }) { view -> MediaTypeRow(view, onOpen) }

        item { SectionHeader(stringResource(R.string.albums_section_utilities), "utilities") }
        item {
            UtilityRows(
                smartAlbumCount = state.smartAlbumCount,
                onOpen = onOpen,
                onOpenSmartAlbums = onOpenSmartAlbums,
                onOpenDuplicates = onOpenDuplicates,
                onOpenPlaces = onOpenPlaces,
                onOpenGated = onOpenGated,
            )
        }

        item { SectionHeader(stringResource(R.string.albums_section_more), "more") }
        item { MoreHint() }
        items(state.viewAlbums, key = { it.id.value }) { album -> AlbumRow(album, onOpen) }
        items(state.federated, key = { it.groupId }) { group -> FederatedRow(group, onOpen) }
    }
}

@Composable
private fun MyAlbumsHeader(onCreateAlbum: () -> Unit) {
    CapsuleSectionHeader(
        title = stringResource(R.string.albums_section_my_albums),
        modifier = Modifier.testTag("${AlbumsTestTags.HUB_SECTION}:my-albums"),
        trailing = {
            CapsuleButton(
                onClick = onCreateAlbum,
                label = stringResource(R.string.albums_action_create),
                emphasis = CapsuleButtonEmphasis.Tertiary,
                icon = Icons.Filled.Add,
            )
        },
    )
}

@Composable
private fun SectionHeader(
    title: String,
    key: String,
) {
    CapsuleSectionHeader(title = title, modifier = Modifier.testTag("${AlbumsTestTags.HUB_SECTION}:$key"))
}

@Composable
private fun EmptyHub(onCreateAlbum: () -> Unit) {
    CapsuleEmptyState(
        title = stringResource(R.string.albums_empty_title),
        body = stringResource(R.string.albums_empty_body),
        action = {
            CapsuleButton(onClick = onCreateAlbum, label = stringResource(R.string.albums_action_create))
        },
    )
}

@Composable
private fun AlbumRow(
    album: HubAlbum,
    onOpen: (CollectionRef) -> Unit,
) {
    val name = if (album.isDefault) stringResource(R.string.albums_default_album) else album.name
    val supporting =
        if (album.isDefault) {
            stringResource(R.string.albums_default_album_supporting)
        } else {
            stringResource(R.string.albums_item_count).withArgs("count" to album.assetCount)
        }
    CapsuleListItem(
        headline = name,
        modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:${album.id.value}"),
        supporting = supporting,
        onClick = { onOpen(album.ref) },
        trailing = { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null) },
        contentDescription = stringResource(R.string.albums_a11y_open_album).withArgs("name" to name),
    )
}

@Composable
private fun MediaTypeRow(
    view: HubMediaType,
    onOpen: (CollectionRef) -> Unit,
) {
    CapsuleListItem(
        headline = systemViewLabel(view.kind),
        modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:${view.kind.token}"),
        supporting = stringResource(R.string.albums_item_count).withArgs("count" to view.count),
        onClick = { onOpen(CollectionRef.SystemView(view.kind)) },
        trailing = { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null) },
    )
}

@Composable
private fun UtilityRows(
    smartAlbumCount: Int,
    onOpen: (CollectionRef) -> Unit,
    onOpenSmartAlbums: () -> Unit,
    onOpenDuplicates: () -> Unit,
    onOpenPlaces: () -> Unit,
    onOpenGated: (GatedSection) -> Unit,
) {
    val locked = stringResource(R.string.albums_utility_locked_hint)
    Column {
        CapsuleListItem(
            headline = stringResource(R.string.android_destination_smart_albums),
            modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:smart-albums"),
            supporting = stringResource(R.string.albums_item_count).withArgs("count" to smartAlbumCount),
            onClick = onOpenSmartAlbums,
        )
        CapsuleListItem(
            headline = stringResource(R.string.albums_utility_stacks),
            modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:stacks"),
            onClick = { onOpen(CollectionRef.Stacks) },
        )
        CapsuleListItem(
            headline = stringResource(R.string.android_destination_duplicates),
            modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:duplicates"),
            onClick = onOpenDuplicates,
        )
        CapsuleListItem(
            headline = stringResource(R.string.android_destination_places),
            modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:places"),
            onClick = onOpenPlaces,
        )
        CapsuleListItem(
            headline = stringResource(R.string.nav_trash),
            modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:trash"),
            supporting = locked,
            leading = { Icon(Icons.Filled.Lock, contentDescription = null) },
            onClick = { onOpenGated(GatedSection.Trash) },
        )
        CapsuleListItem(
            headline = stringResource(R.string.nav_hidden),
            modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:hidden"),
            supporting = locked,
            leading = { Icon(Icons.Filled.Lock, contentDescription = null) },
            onClick = { onOpenGated(GatedSection.Hidden) },
        )
    }
}

@Composable
private fun MoreHint() {
    Text(
        text = stringResource(R.string.albums_more_views_hint),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = HINT_INSET, vertical = HINT_INSET / 2),
    )
}

@Composable
private fun FederatedRow(
    group: FederatedAlbum,
    onOpen: (CollectionRef) -> Unit,
) {
    CapsuleListItem(
        headline = group.name,
        modifier = Modifier.testTag("${AlbumsTestTags.HUB_ALBUM}:group-${group.groupId}"),
        supporting = stringResource(R.string.albums_item_count).withArgs("count" to group.visibleAssetCount),
        onClick = { onOpen(CollectionRef.Federated(group.groupId)) },
        trailing = { CapsuleBadge(label = stringResource(R.string.albums_federated_badge)) },
    )
}

/** The gutter the explanatory line under a section header sits in. */
private val HINT_INSET = 16.dp
