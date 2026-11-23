package com.justin13888.capsule.feature.people

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Face
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.designsystem.theme.CapsuleShapes
import com.justin13888.capsule.core.i18n.R

/** Test handle for the stale-grouping notice, findable regardless of its wording. */
const val PEOPLE_STALE_NOTICE_TAG: String = "people.staleNotice"

/** Test handle for the undo offered after a person is hidden. */
const val PEOPLE_UNDO_TAG: String = "people.undo"

/** Test handle for the in-place message shown when an action is refused. */
const val PEOPLE_ACTION_FAILURE_TAG: String = "people.actionFailure"

private val ScreenPadding = 16.dp
private val ScreenSpacing = 12.dp
private val CoverSize = 56.dp

/** The alpha byte of an opaque `0xRRGGBB` fill. */
private const val OPAQUE_ALPHA = 0xFF000000.toInt()

/**
 * Everything the People hub can ask for, in one object.
 *
 * Grouped so the screen stays a plain function of state plus intent: eleven callback
 * parameters would push the composable well past the project's limit and would make every
 * preview and every test restate all eleven.
 */
@Suppress("LongParameterList")
@Immutable
class PeopleActions(
    /** A person's own screen was opened. */
    val onOpenPerson: (String) -> Unit,
    /** A photograph was opened. */
    val onOpenAsset: (String) -> Unit,
    /** The naming sheet was asked for. */
    val onNameRequested: (NameTarget) -> Unit,
    /** A name was typed. */
    val onNameSubmit: (String) -> Unit,
    /** The grouping being named is somebody already named. */
    val onAttachCluster: (String) -> Unit,
    /** The merge sheet was asked for. */
    val onMergeOpen: (PersonCard) -> Unit,
    /** A merge target was chosen. */
    val onMergeInto: (String) -> Unit,
    /** Both sheets closed. */
    val onDismissSheets: () -> Unit,
    /** A person was hidden. */
    val onHide: (PersonCard) -> Unit,
    /** The hide was taken back. */
    val onUndoHide: () -> Unit,
    /** A smart album was expanded. */
    val onOpenSmartAlbum: (SmartAlbumKind) -> Unit,
    /** The expanded smart album was collapsed. */
    val onCloseSmartAlbum: () -> Unit,
    /** The in-place failure message was acknowledged. */
    val onDismissActionFailure: () -> Unit,
    /** The remediation on the error state. */
    val onRetry: () -> Unit,
) {
    companion object {
        /** A set that does nothing, for previews and for state-only tests. */
        val Inert: PeopleActions =
            PeopleActions({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})
    }
}

/**
 * The People hub: who the user has named, what has been grouped but not named, and the
 * albums the models built.
 *
 * Stateless by construction, so every state it can be in — including a library whose face
 * groupings are being withheld from queries because a model version changed — is one line to
 * reach from a preview or a test.
 */
@Composable
fun PeopleScreen(
    state: PeopleUiState,
    actions: PeopleActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(title = stringResource(R.string.android_destination_people))
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            when (state.phase) {
                PeoplePhase.Loading ->
                    CapsuleLoadingState(
                        modifier = Modifier.fillMaxSize(),
                        title = stringResource(R.string.people_loading),
                        contentDescription = stringResource(R.string.people_loading),
                    )

                PeoplePhase.Failed ->
                    CapsuleErrorState(
                        title = failureTitle(state.failure ?: PeopleFailure.Unknown),
                        modifier = Modifier.fillMaxSize(),
                        body = failureBody(state.failure ?: PeopleFailure.Unknown),
                        action = {
                            CapsuleButton(
                                onClick = actions.onRetry,
                                label = stringResource(R.string.people_error_retry),
                                emphasis = CapsuleButtonEmphasis.Secondary,
                            )
                        },
                    )

                PeoplePhase.Empty ->
                    CapsuleEmptyState(
                        title = stringResource(R.string.people_empty_title),
                        modifier = Modifier.fillMaxSize(),
                        body = stringResource(R.string.people_empty_body),
                        icon = Icons.Filled.Face,
                    )

                PeoplePhase.Content ->
                    if (state.openedSmartAlbum == null) {
                        HubList(state = state, actions = actions)
                    } else {
                        OpenedSmartAlbum(state = state, kind = state.openedSmartAlbum, actions = actions)
                    }
            }
        }
    }
    state.nameTarget?.let { target ->
        PeopleNameSheet(
            target = target,
            people = state.people,
            onSubmit = actions.onNameSubmit,
            onAttach = actions.onAttachCluster,
            onDismiss = actions.onDismissSheets,
        )
    }
    state.mergeSource?.let { source ->
        PeopleMergeSheet(
            source = source,
            candidates = state.people,
            onMergeInto = actions.onMergeInto,
            onDismiss = actions.onDismissSheets,
        )
    }
}

/** The hub proper: notices, then people, then groupings, then the built albums. */
@Composable
private fun HubList(
    state: PeopleUiState,
    actions: PeopleActions,
) {
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(ScreenSpacing / 2),
    ) {
        Notices(state = state, actions = actions)
        if (state.people.isNotEmpty()) {
            CapsuleSectionHeader(title = stringResource(R.string.people_hub_named))
            state.people.forEach { person -> PersonRow(person = person, actions = actions) }
        }
        if (state.clusters.isNotEmpty()) {
            CapsuleSectionHeader(title = stringResource(R.string.people_hub_unnamed))
            Text(
                text = stringResource(R.string.people_hub_unnamed_body),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = ScreenPadding),
            )
            state.clusters.forEach { cluster -> ClusterRow(cluster = cluster, actions = actions) }
        }
        CapsuleSectionHeader(title = stringResource(R.string.people_smart_albums_title))
        Text(
            text = stringResource(R.string.people_smart_albums_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = ScreenPadding),
        )
        state.smartAlbums.forEach { album -> SmartAlbumRow(album = album, actions = actions) }
    }
}

/**
 * The three conditions the hub reports in place rather than by replacing itself.
 *
 * All three are conditions rather than events, which is why they are banners and not
 * snackbars: a stale model slot lasts until regeneration finishes, an undo has to survive
 * long enough to be found, and a refused write is worth reading twice.
 */
@Composable
private fun Notices(
    state: PeopleUiState,
    actions: PeopleActions,
) {
    state.actionFailure?.let { failure ->
        CapsuleBanner(
            title = failureTitle(failure),
            modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenPadding).testTag(PEOPLE_ACTION_FAILURE_TAG),
            body = failureBody(failure),
            severity = CapsuleBannerSeverity.Error,
            action = {
                CapsuleButton(
                    onClick = actions.onDismissActionFailure,
                    label = stringResource(R.string.common_close),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                )
            },
        )
    }
    state.hidden?.let { hidden ->
        CapsuleBanner(
            title = stringResource(R.string.people_hidden_notice).withArguments("name" to hidden.displayName),
            modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenPadding).testTag(PEOPLE_UNDO_TAG),
            severity = CapsuleBannerSeverity.Info,
            action = {
                CapsuleButton(
                    onClick = actions.onUndoHide,
                    label = stringResource(R.string.people_hidden_undo),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                )
            },
        )
    }
    state.stale?.let { stale ->
        CapsuleBanner(
            title = stringResource(R.string.people_stale_title),
            modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenPadding).testTag(PEOPLE_STALE_NOTICE_TAG),
            body =
                stringResource(R.string.people_stale_body).withArguments("count" to stale.staleCount) +
                    " " + stale.slots.joinToString(),
            severity = CapsuleBannerSeverity.Degraded,
        )
    }
}

/** One named person, with the three things the hub lets the user do to them. */
@Composable
private fun PersonRow(
    person: PersonCard,
    actions: PeopleActions,
) {
    CapsuleCard(modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenPadding)) {
        CapsuleListItem(
            headline = person.displayName,
            supporting = photoCountLabel(person.photoCount),
            leading = { CoverTile(person.cover) },
            onClick = { actions.onOpenPerson(person.personId) },
            contentDescription = personDescription(person),
        )
        // Scrolls rather than wraps so the three actions survive a 200% font scale intact:
        // a wrapped row would push the last one out of the card entirely.
        Row(
            modifier =
                Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = ScreenPadding / 2),
            horizontalArrangement = Arrangement.spacedBy(ScreenSpacing / 2),
        ) {
            CapsuleButton(
                onClick = {
                    actions.onNameRequested(
                        NameTarget(personId = person.personId, clusterId = null, initialName = person.displayName),
                    )
                },
                label = stringResource(R.string.people_action_rename),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
            CapsuleButton(
                onClick = { actions.onMergeOpen(person) },
                label = stringResource(R.string.people_action_merge),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
            CapsuleButton(
                onClick = { actions.onHide(person) },
                label = stringResource(R.string.people_action_hide),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        }
    }
}

/**
 * One grouping nobody has named.
 *
 * A grouping from a superseded model version is listed here rather than dropped, with the
 * notice above explaining why it is not answering queries — a person who quietly vanished
 * after an app update is the outcome this row exists to prevent.
 */
@Composable
private fun ClusterRow(
    cluster: ClusterCard,
    actions: PeopleActions,
) {
    CapsuleListItem(
        headline = stringResource(R.string.people_a11y_cluster).withArguments("count" to cluster.photoCount),
        modifier = Modifier.padding(horizontal = ScreenPadding),
        leading = { CoverTile(cluster.cover) },
        trailing = {
            CapsuleButton(
                onClick = {
                    actions.onNameRequested(
                        NameTarget(personId = null, clusterId = cluster.clusterId, initialName = ""),
                    )
                },
                label = stringResource(R.string.people_action_name),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

/** One built album. Its count is computed from the library every load, never stored. */
@Composable
private fun SmartAlbumRow(
    album: SmartAlbumCard,
    actions: PeopleActions,
) {
    val name = smartAlbumLabel(album.kind)
    CapsuleListItem(
        headline = name,
        modifier = Modifier.padding(horizontal = ScreenPadding),
        supporting = stringResource(R.string.people_smart_album_count).withArguments("count" to album.photoCount),
        leading = { CoverTile(album.cover) },
        onClick = { actions.onOpenSmartAlbum(album.kind) },
        contentDescription =
            stringResource(R.string.people_a11y_smart_album)
                .withArguments("name" to name, "count" to album.photoCount),
    )
}

/** One expanded album, drawn in the shared gallery. */
@Composable
private fun OpenedSmartAlbum(
    state: PeopleUiState,
    kind: SmartAlbumKind,
    actions: PeopleActions,
) {
    val name = smartAlbumLabel(kind)
    Column(modifier = Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenPadding),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(text = name, style = MaterialTheme.typography.titleMediumEmphasized, modifier = Modifier.weight(1f))
            IconButton(onClick = actions.onCloseSmartAlbum) {
                Icon(imageVector = Icons.Filled.Close, contentDescription = stringResource(R.string.common_close))
            }
        }
        val photoTemplate = stringResource(R.string.people_a11y_photo)
        val total = state.openedSmartAlbumPhotos.size
        if (total == 0) {
            CapsuleEmptyState(
                title = stringResource(R.string.people_person_empty),
                modifier = Modifier.fillMaxSize(),
                icon = Icons.Filled.Face,
            )
        } else {
            PeopleGrid(
                photos = state.openedSmartAlbumPhotos,
                contentDescription = stringResource(R.string.people_a11y_grid).withArguments("name" to name),
                cellDescription = { index ->
                    photoTemplate.withArguments("index" to index + 1, "count" to total)
                },
                onOpenAsset = actions.onOpenAsset,
                modifier = Modifier.weight(1f).fillMaxWidth(),
            )
        }
    }
}

/**
 * A face tile.
 *
 * Filled with the asset's own dominant colour, which rides inside the encrypted metadata and
 * so is available before any blob has been fetched. A person with no cover in reach gets the
 * container tone rather than a broken-image mark: there is nothing wrong, the thumbnail has
 * simply not arrived.
 */
@Composable
private fun CoverTile(photo: PersonPhoto?) {
    val fill =
        if (photo == null) {
            MaterialTheme.colorScheme.surfaceVariant
        } else {
            Color(OPAQUE_ALPHA or photo.placeholder)
        }
    Box(modifier = Modifier.size(CoverSize).clip(CapsuleShapes.medium).background(fill))
}
