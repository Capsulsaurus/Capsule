package com.justin13888.capsule.core.designsystem.component

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material3.FloatingActionButtonMenu
import androidx.compose.material3.FloatingActionButtonMenuItem
import androidx.compose.material3.FloatingToolbarDefaults
import androidx.compose.material3.HorizontalFloatingToolbar
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.ToggleFloatingActionButton
import androidx.compose.material3.ToggleFloatingActionButtonDefaults
import androidx.compose.material3.VerticalFloatingToolbar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import kotlinx.collections.immutable.ImmutableList

/** Which way a [CapsuleFloatingToolbar] lays its actions out. */
enum class CapsuleToolbarOrientation {
    /** Along the bottom edge, under the thumb. The default on a phone. */
    Horizontal,

    /** Down a side edge, clear of the content. The default on a tablet in landscape. */
    Vertical,
}

/**
 * A toolbar that floats over the content instead of displacing it.
 *
 * The whole premise of this app is that the photograph is the interface, so the editing
 * and selection controls sit on a small island above it rather than in a bar that steals
 * a strip of the image. It collapses as the user scrolls and expands when they stop,
 * which is why [expanded] is hoisted: the scroll state that drives it belongs to the
 * screen, not to the toolbar.
 *
 * @param vibrant uses the high-emphasis container. Reserve it for a destructive or modal
 *   context — a selection toolbar, a crop session — where the toolbar must not be
 *   mistaken for part of the photograph underneath it.
 */
@Composable
fun CapsuleFloatingToolbar(
    expanded: Boolean,
    modifier: Modifier = Modifier,
    orientation: CapsuleToolbarOrientation = CapsuleToolbarOrientation.Horizontal,
    vibrant: Boolean = false,
    content: @Composable () -> Unit,
) {
    val colors =
        if (vibrant) {
            FloatingToolbarDefaults.vibrantFloatingToolbarColors()
        } else {
            FloatingToolbarDefaults.standardFloatingToolbarColors()
        }
    when (orientation) {
        CapsuleToolbarOrientation.Horizontal ->
            HorizontalFloatingToolbar(expanded = expanded, modifier = modifier, colors = colors) {
                content()
            }

        CapsuleToolbarOrientation.Vertical ->
            VerticalFloatingToolbar(expanded = expanded, modifier = modifier, colors = colors) {
                content()
            }
    }
}

/**
 * One entry in a [CapsuleFabMenu].
 *
 * @param label already-localized text. Menu items are always labelled: an icon-only FAB
 *   menu is a memory test.
 */
@Immutable
class CapsuleFabMenuItem(
    val label: String,
    val icon: ImageVector,
    val onClick: () -> Unit,
)

/**
 * The primary action, with its siblings unfolding above it.
 *
 * Preferred over a bare FAB anywhere the "add" gesture is genuinely several gestures —
 * import from the camera roll, import from a card reader, start a tethered session. The
 * alternative, a bottom sheet of choices, costs a full screen transition for what is
 * usually a two-tap decision.
 *
 * @param toggleDescription announced label for the FAB itself, which has no visible text.
 */
@Composable
fun CapsuleFabMenu(
    expanded: Boolean,
    onExpandedChange: (Boolean) -> Unit,
    items: ImmutableList<CapsuleFabMenuItem>,
    modifier: Modifier = Modifier,
    icon: ImageVector = Icons.Filled.Add,
    toggleDescription: String? = null,
) {
    FloatingActionButtonMenu(
        expanded = expanded,
        button = {
            ToggleFloatingActionButton(
                checked = expanded,
                onCheckedChange = onExpandedChange,
                modifier = Modifier.capsuleTouchTarget().capsuleDescription(toggleDescription),
            ) {
                val progress = checkedProgress
                Icon(
                    imageVector = icon,
                    contentDescription = null,
                    modifier =
                        with(ToggleFloatingActionButtonDefaults) {
                            Modifier.animateIcon({ progress })
                        },
                )
            }
        },
        modifier = modifier,
    ) {
        items.forEach { item ->
            FloatingActionButtonMenuItem(
                onClick = item.onClick,
                text = { Text(text = item.label) },
                icon = { Icon(imageVector = item.icon, contentDescription = null) },
            )
        }
    }
}
