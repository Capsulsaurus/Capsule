package com.justin13888.capsule.feature.timeline

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.LibraryAdd
import androidx.compose.material.icons.filled.RateReview
import androidx.compose.material.icons.filled.SelectAll
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.VisibilityOff
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleFloatingToolbar
import com.justin13888.capsule.core.designsystem.component.CapsuleTooltip
import com.justin13888.capsule.core.designsystem.component.capsuleTouchTarget
import com.justin13888.capsule.core.i18n.R

/** One action on the selection toolbar, resolved to the strings the platform announces. */
@Immutable
private data class ToolbarAction(
    val action: TimelineSelectionAction,
    val icon: ImageVector,
    val label: String,
    val enabled: Boolean,
    val disabledReason: String?,
)

private val ToolbarInset = 8.dp

/**
 * What a selection can be done to, floating over the grid.
 *
 * A floating toolbar rather than a bottom app bar because selection is a mode over the
 * photographs, not a different screen: displacing the grid by a bar's height would reflow
 * every tile the moment the user long-pressed, moving the very thing they were aiming at.
 * It is drawn vibrant, which the design system reserves for a modal or destructive context —
 * selection is both, and a toolbar the user could mistake for part of the photograph
 * underneath it would be a toolbar they delete with.
 *
 * Share is disabled rather than hidden when the device is offline. A control that vanishes
 * teaches the user nothing; one that stays and says why teaches them when to come back.
 *
 * @param count how many assets are selected, for the toolbar's own title.
 * @param isOffline whether the actions that need a server are reachable.
 * @param onAction invoked with the action pressed; the caller supplies the selection.
 * @param onSelectAll extends the selection to the whole library.
 * @param onDismiss leaves selection mode.
 */
@Suppress("LongParameterList")
@Composable
fun SelectionToolbar(
    count: Int,
    isOffline: Boolean,
    onAction: (TimelineSelectionAction) -> Unit,
    onSelectAll: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val title = stringResource(R.string.timeline_selection_count).withArguments("count" to count)
    val toolbarLabel = stringResource(R.string.timeline_selection_a11y_toolbar)
    val actions = toolbarActions(isOffline)
    CapsuleFloatingToolbar(
        expanded = true,
        modifier =
            modifier
                .testTag(TimelineTestTags.SELECTION_TOOLBAR)
                .semantics { contentDescription = toolbarLabel },
        vibrant = true,
    ) {
        Row(
            modifier = Modifier.padding(horizontal = ToolbarInset),
            horizontalArrangement = Arrangement.spacedBy(ToolbarInset / 2),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onDismiss, modifier = Modifier.capsuleTouchTarget()) {
                Icon(
                    imageVector = Icons.Filled.Close,
                    contentDescription = stringResource(R.string.timeline_selection_done),
                )
            }
            Text(text = title)
            CapsuleTooltip(text = stringResource(R.string.timeline_selection_select_all)) {
                IconButton(onClick = onSelectAll, modifier = Modifier.capsuleTouchTarget()) {
                    Icon(
                        imageVector = Icons.Filled.SelectAll,
                        contentDescription = stringResource(R.string.timeline_selection_select_all),
                    )
                }
            }
            actions.forEach { entry ->
                CapsuleTooltip(text = entry.disabledReason ?: entry.label) {
                    IconButton(
                        onClick = { onAction(entry.action) },
                        modifier = Modifier.capsuleTouchTarget(),
                        enabled = entry.enabled,
                    ) {
                        Icon(imageVector = entry.icon, contentDescription = entry.label)
                    }
                }
            }
        }
    }
}

/**
 * The six actions, in the order a photographer reaches for them.
 *
 * Share first because it is the one that leaves the app, delete last because it is the one
 * that cannot be taken back by pressing it again.
 */
@Composable
private fun toolbarActions(isOffline: Boolean): List<ToolbarAction> {
    val offlineReason = stringResource(R.string.timeline_action_share_offline)
    return listOf(
        ToolbarAction(
            action = TimelineSelectionAction.Share,
            icon = Icons.Filled.Share,
            label = stringResource(R.string.common_share),
            enabled = !isOffline,
            disabledReason = offlineReason.takeIf { isOffline },
        ),
        ToolbarAction(
            action = TimelineSelectionAction.AddToAlbum,
            icon = Icons.Filled.LibraryAdd,
            label = stringResource(R.string.timeline_action_add_to_album),
            enabled = true,
            disabledReason = null,
        ),
        ToolbarAction(
            action = TimelineSelectionAction.Favourite,
            icon = Icons.Filled.Favorite,
            label = stringResource(R.string.timeline_action_favourite),
            enabled = true,
            disabledReason = null,
        ),
        ToolbarAction(
            action = TimelineSelectionAction.Hide,
            icon = Icons.Filled.VisibilityOff,
            label = stringResource(R.string.timeline_action_hide),
            enabled = true,
            disabledReason = null,
        ),
        ToolbarAction(
            action = TimelineSelectionAction.Cull,
            icon = Icons.Filled.RateReview,
            label = stringResource(R.string.timeline_action_cull),
            enabled = true,
            disabledReason = null,
        ),
        ToolbarAction(
            action = TimelineSelectionAction.Delete,
            icon = Icons.Filled.Delete,
            label = stringResource(R.string.common_delete),
            enabled = true,
            disabledReason = null,
        ),
    )
}
