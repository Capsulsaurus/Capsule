package com.justin13888.capsule.core.designsystem.component

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.snap
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.ButtonGroup
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SplitButton
import androidx.compose.material3.SplitButtonDefaults
import androidx.compose.material3.ToggleButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.vector.ImageVector
import com.justin13888.capsule.core.designsystem.theme.LocalReduceMotion
import kotlinx.collections.immutable.ImmutableList

/** The rotation applied to a split button's chevron when its menu is open. */
private const val CHEVRON_OPEN_DEGREES = 180f

/**
 * One action inside a [CapsuleButtonGroup].
 *
 * A value type rather than a slot lambda because the group has to be able to *measure*
 * its items to decide what overflows into the menu, and it can only do that if it can
 * enumerate them before composing them.
 *
 * @param label already-localized text.
 */
@Immutable
class CapsuleButtonGroupItem(
    val label: String,
    val onClick: () -> Unit,
    val icon: ImageVector? = null,
    val enabled: Boolean = true,
)

/**
 * A row of related actions that squeeze against each other on press and spill into an
 * overflow menu when the row runs out of width.
 *
 * This is the app's answer to a contextual action bar. Selection toolbars in a photo
 * library grow and shrink with what is selected, and a fixed row either truncates on a
 * phone or looks empty on a tablet; the group's own overflow handling is the reason to
 * reach for it instead of a hand-rolled `Row`.
 *
 * @param overflowDescription announced label for the overflow affordance.
 */
@Composable
fun CapsuleButtonGroup(
    items: ImmutableList<CapsuleButtonGroupItem>,
    modifier: Modifier = Modifier,
    overflowDescription: String? = null,
) {
    ButtonGroup(
        overflowIndicator = { menuState ->
            IconButton(
                onClick = { if (menuState.isShowing) menuState.dismiss() else menuState.show() },
                modifier = Modifier.capsuleTouchTarget(),
            ) {
                Icon(imageVector = Icons.Filled.MoreVert, contentDescription = overflowDescription)
            }
        },
        modifier = modifier,
    ) {
        items.forEach { item ->
            clickableItem(
                onClick = item.onClick,
                label = item.label,
                icon = item.icon?.let { { Icon(imageVector = it, contentDescription = null) } },
                enabled = item.enabled,
            )
        }
    }
}

/**
 * A button that stays pressed.
 *
 * Used for filters and view modes — anything where the current state has to survive the
 * finger lifting. The expressive shape change between checked and unchecked is doing the
 * work here: in a toolbar over a photograph, a tint alone disappears against whatever
 * the photograph happens to be.
 *
 * @param label already-localized text.
 * @param contentDescription overrides the announced label; the checked state is
 *   announced by the platform and must not be repeated here.
 */
@Composable
fun CapsuleToggleButton(
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    label: String,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    enabled: Boolean = true,
    contentDescription: String? = null,
) {
    ToggleButton(
        checked = checked,
        onCheckedChange = onCheckedChange,
        modifier = modifier.capsuleTouchTarget().capsuleDescription(contentDescription),
        enabled = enabled,
    ) {
        CapsuleLabelWithIcon(label = label, icon = icon)
    }
}

/**
 * A primary action with its variants one tap away.
 *
 * The split is worth its complexity exactly where a default is right most of the time
 * but not always — "Export" with a menu of presets, "Share" with a menu of link scopes.
 * The caller owns the menu; this wrapper owns the two halves and the chevron.
 *
 * @param label already-localized text for the leading half. No icon slot: the leading
 *   half is already competing with the chevron for width, and an icon there pushes the
 *   label into an ellipsis on a phone.
 * @param expanded whether the caller's menu is showing. Hoisted, because the menu is
 *   anchored and dismissed by the caller.
 * @param menuDescription announced label for the trailing half.
 */
@Composable
fun CapsuleSplitButton(
    onClick: () -> Unit,
    label: String,
    expanded: Boolean,
    onExpandedChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    menuDescription: String? = null,
) {
    val reduceMotion = LocalReduceMotion.current
    val rotation by animateFloatAsState(
        targetValue = if (expanded) CHEVRON_OPEN_DEGREES else 0f,
        animationSpec = if (reduceMotion) snap() else MaterialTheme.motionScheme.fastSpatialSpec(),
        label = "capsule-split-button-chevron",
    )
    SplitButton(
        leadingButton = {
            SplitButtonDefaults.LeadingButton(onClick = onClick, enabled = enabled) {
                CapsuleLabelWithIcon(label = label, icon = null)
            }
        },
        trailingButton = {
            SplitButtonDefaults.TrailingButton(
                checked = expanded,
                onCheckedChange = onExpandedChange,
                enabled = enabled,
                modifier = Modifier.capsuleDescription(menuDescription),
            ) {
                Icon(
                    imageVector = Icons.Filled.KeyboardArrowDown,
                    contentDescription = null,
                    modifier = Modifier.rotate(rotation),
                )
            }
        },
        modifier = modifier.capsuleTouchTarget(),
    )
}
