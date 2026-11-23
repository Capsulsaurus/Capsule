package com.justin13888.capsule.feature.drops.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Inbox
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
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
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.DropId
import com.justin13888.capsule.feature.drops.AlbumOption
import com.justin13888.capsule.feature.drops.DropInboxUiState
import com.justin13888.capsule.feature.drops.DropRow
import com.justin13888.capsule.feature.drops.SEPARATOR
import com.justin13888.capsule.feature.drops.failureMessage
import com.justin13888.capsule.feature.drops.failureTitle
import com.justin13888.capsule.feature.drops.formattedBytes
import com.justin13888.capsule.feature.drops.formattedDate
import com.justin13888.capsule.feature.drops.withArguments

/** Test tag prefix on one pending deposit's card; the drop id is appended. */
const val DROP_ROW_TAG: String = "drop-row-"

/** Test tag prefix on a deposit's adopt action. */
const val DROP_ADOPT_TAG: String = "drop-adopt-"

/** Test tag prefix on a deposit's discard action. */
const val DROP_DISCARD_TAG: String = "drop-discard-"

/** Test tag on the banner that says nothing here has been applied. */
const val DROP_REVIEW_BANNER_TAG: String = "drop-review-banner"

/** Test tag on the album picker that adoption goes through. */
const val DROP_ALBUM_PICKER_TAG: String = "drop-album-picker"

/** Test tag on the discard confirmation's own action, which shares its label with the row. */
const val DROP_DISCARD_CONFIRM_TAG: String = "drop-discard-confirm"

private val ListPadding = 16.dp
private val CardSpacing = 8.dp

/** Everything the inbox can be asked to do. */
@Immutable
class DropInboxActions(
    /** Opens the album picker for one deposit. */
    val beginAdoption: (DropId) -> Unit,
    /** Closes the album picker without adopting. */
    val cancelAdoption: () -> Unit,
    /** Adopts a deposit into an album. */
    val adopt: (DropId, AlbumId) -> Unit,
    /** Discards a deposit, releasing its bytes. */
    val discard: (DropId) -> Unit,
    /** Re-reads the inbox after a failure. */
    val retry: () -> Unit,
    /** Leaves the inbox. */
    val back: () -> Unit,
)

/**
 * The inbox of files guests deposited.
 *
 * A **review surface**, and the screen says so at the top rather than assuming the user
 * knows. Nothing listed here is in the library, nothing is applied automatically, and every
 * field a row shows — the filename most of all — is the guest's own unverified claim. The
 * two actions on a row are the only two futures a deposit has.
 */
@Composable
fun DropInboxScreen(
    state: DropInboxUiState,
    actions: DropInboxActions,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            CapsuleTopAppBar(
                title = stringResource(R.string.android_destination_drop_inbox),
                subtitle = stringResource(R.string.android_drops_inbox_subtitle),
                navigationIcon = { BackButton(actions.back) },
            )
        },
    ) { insets ->
        Column(modifier = Modifier.padding(insets).fillMaxSize()) {
            when {
                state.isLoading ->
                    CapsuleLoadingState(
                        title = stringResource(R.string.android_drops_inbox_loading),
                        contentDescription = stringResource(R.string.android_drops_inbox_loading),
                    )

                state.failure != null && state.drops.isEmpty() ->
                    CapsuleErrorState(
                        title = failureTitle(state.failure),
                        body = failureMessage(state.failure),
                        action = {
                            CapsuleButton(onClick = actions.retry, label = stringResource(R.string.android_drops_retry))
                        },
                    )

                state.isEmpty ->
                    CapsuleEmptyState(
                        title = stringResource(R.string.android_drops_inbox_empty_title),
                        body = stringResource(R.string.android_drops_inbox_empty_body),
                        icon = Icons.Filled.Inbox,
                    )

                else -> InboxList(state, actions)
            }
        }
    }
    val pending = state.pendingAdoption
    if (pending != null) {
        AlbumPicker(
            row = pending,
            albums = state.albums,
            onPick = { albumId -> actions.adopt(pending.drop.id, albumId) },
            onDismiss = actions.cancelAdoption,
        )
    }
}

@Composable
private fun InboxList(
    state: DropInboxUiState,
    actions: DropInboxActions,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(ListPadding),
        verticalArrangement = Arrangement.spacedBy(CardSpacing),
    ) {
        item {
            CapsuleBanner(
                title = stringResource(R.string.android_drops_review_banner_title),
                body = stringResource(R.string.android_drops_review_banner_body),
                severity = CapsuleBannerSeverity.Warning,
                modifier = Modifier.fillMaxWidth().testTag(DROP_REVIEW_BANNER_TAG),
            )
        }
        if (state.failure != null) {
            item {
                CapsuleBanner(
                    title = failureTitle(state.failure),
                    body = failureMessage(state.failure),
                    severity = CapsuleBannerSeverity.Error,
                    modifier = Modifier.fillMaxWidth(),
                    action = {
                        CapsuleButton(
                            onClick = actions.retry,
                            label = stringResource(R.string.android_drops_retry),
                            emphasis = CapsuleButtonEmphasis.Tertiary,
                        )
                    },
                )
            }
        }
        items(items = state.drops, key = { it.drop.id.value }) { row ->
            DropCard(row = row, state = state, actions = actions)
        }
    }
}

@Composable
private fun DropCard(
    row: DropRow,
    state: DropInboxUiState,
    actions: DropInboxActions,
) {
    var confirmingDiscard by remember(row.drop.id) { mutableStateOf(false) }
    val busy = state.busyDropId == row.drop.id
    CapsuleCard(modifier = Modifier.fillMaxWidth().testTag(DROP_ROW_TAG + row.drop.id.value)) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(ListPadding),
            verticalArrangement = Arrangement.spacedBy(CardSpacing / 2),
        ) {
            Text(text = dropName(row), style = MaterialTheme.typography.titleMedium)
            Text(
                text = stringResource(R.string.android_drops_name_unverified),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = dropFacts(row),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(CardSpacing)) {
                CapsuleButton(
                    onClick = { actions.beginAdoption(row.drop.id) },
                    label = stringResource(R.string.android_drops_adopt),
                    enabled = !busy && state.canAdopt,
                    modifier = Modifier.testTag(DROP_ADOPT_TAG + row.drop.id.value),
                )
                CapsuleButton(
                    onClick = { confirmingDiscard = true },
                    label = stringResource(R.string.android_drops_discard),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                    enabled = !busy,
                    modifier = Modifier.testTag(DROP_DISCARD_TAG + row.drop.id.value),
                )
            }
            if (!state.canAdopt) {
                Text(
                    text = stringResource(R.string.android_drops_adopt_no_albums),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
        }
    }
    if (confirmingDiscard) {
        DiscardConfirmation(
            onConfirm = {
                confirmingDiscard = false
                actions.discard(row.drop.id)
            },
            onDismiss = { confirmingDiscard = false },
        )
    }
}

/**
 * Where a deposit is going, and what will be written about where it came from.
 *
 * The origin note is quoted verbatim in the dialog rather than described, because it is the
 * one thing adoption records that the user did not author. It is explicitly unverified and
 * self-asserted: the guest signed nothing, and the adopter — not the guest — becomes the
 * asset's author the moment this is confirmed.
 */
@Composable
private fun AlbumPicker(
    row: DropRow,
    albums: List<AlbumOption>,
    onPick: (AlbumId) -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        modifier = Modifier.testTag(DROP_ALBUM_PICKER_TAG),
        title = { Text(text = stringResource(R.string.android_drops_adopt_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(CardSpacing)) {
                Text(text = stringResource(R.string.android_drops_adopt_body))
                Text(
                    text = guestOriginNote(row),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                albums.forEach { album ->
                    DropsRadioRow(
                        label = albumLabel(album),
                        selected = false,
                        onSelect = { onPick(album.id) },
                    )
                }
            }
        },
        confirmButton = {},
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(text = stringResource(R.string.common_cancel)) }
        },
    )
}

@Composable
private fun DiscardConfirmation(
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(text = stringResource(R.string.android_drops_discard_confirm_title)) },
        text = { Text(text = stringResource(R.string.android_drops_discard_confirm_body)) },
        confirmButton = {
            TextButton(onClick = onConfirm, modifier = Modifier.testTag(DROP_DISCARD_CONFIRM_TAG)) {
                Text(text = stringResource(R.string.android_drops_discard_confirm_action))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(text = stringResource(R.string.common_cancel)) }
        },
    )
}

/** The guest's own name for the file, or a standing label when they supplied none. */
@Composable
internal fun dropName(row: DropRow): String =
    row.drop.descriptor.suggestedFilename
        ?.takeIf(String::isNotBlank)
        ?: stringResource(R.string.android_drops_unnamed)

/** The facts about a deposit that are not the guest's to assert. */
@Composable
internal fun dropFacts(row: DropRow): String {
    val size = formattedBytes(row.drop.descriptor.plaintextSize)
    val received =
        stringResource(R.string.android_drops_received).withArguments("date" to formattedDate(row.drop.receivedAt))
    return listOf(received, size).joinToString(SEPARATOR)
}

/** The unverified, self-asserted note adoption records about a deposit's origin. */
@Composable
internal fun guestOriginNote(row: DropRow): String {
    val date = formattedDate(row.drop.receivedAt)
    val link = row.viaLinkOpaqueId
    return if (link == null) {
        stringResource(R.string.android_drops_origin_note_unknown_link).withArguments("date" to date)
    } else {
        stringResource(R.string.android_drops_origin_note).withArguments("link" to link, "date" to date)
    }
}

/** An album's own name, or the standing name for the nameless default container. */
@Composable
internal fun albumLabel(album: AlbumOption): String =
    album.name.takeIf(String::isNotBlank) ?: stringResource(R.string.android_drops_album_default)
