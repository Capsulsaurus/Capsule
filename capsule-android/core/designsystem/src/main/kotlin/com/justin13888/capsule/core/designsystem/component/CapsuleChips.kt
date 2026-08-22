package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.collections.immutable.ImmutableList

/** The gap between chips in a [CapsuleFilterChipRow]. */
private val ChipSpacing = 8.dp

/**
 * A filter, on or off.
 *
 * Only the filter variant is exposed. Assist and suggestion chips look almost identical
 * and behave nothing alike, and a design system that offers all three ends up with
 * screens where a tap sometimes navigates and sometimes toggles.
 *
 * @param label already-localized text.
 * @param contentDescription overrides the announcement; the selected state is announced
 *   by the platform and must not be spelled out again here.
 */
@Composable
fun CapsuleChip(
    label: String,
    selected: Boolean,
    onSelectedChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    enabled: Boolean = true,
    contentDescription: String? = null,
) {
    FilterChip(
        selected = selected,
        onClick = { onSelectedChange(!selected) },
        label = { Text(text = label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        modifier = modifier.capsuleTouchTarget().capsuleDescription(contentDescription),
        enabled = enabled,
        leadingIcon = icon?.let { { Icon(imageVector = it, contentDescription = null) } },
    )
}

/** One chip's state inside a [CapsuleFilterChipRow]. */
@Immutable
class CapsuleChipState(
    val label: String,
    val selected: Boolean,
    val onSelectedChange: (Boolean) -> Unit,
    val icon: ImageVector? = null,
    val enabled: Boolean = true,
)

/**
 * The row of filters above a grid.
 *
 * Scrolls rather than wraps. A search over a terabyte library can carry a dozen active
 * facets, and a wrapping row would reflow the grid underneath it every time one is
 * toggled — the one thing that must not move while a photographer is scanning thumbnails.
 */
@Composable
fun CapsuleFilterChipRow(
    chips: ImmutableList<CapsuleChipState>,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier.horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(ChipSpacing),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        chips.forEach { chip ->
            CapsuleChip(
                label = chip.label,
                selected = chip.selected,
                onSelectedChange = chip.onSelectedChange,
                icon = chip.icon,
                enabled = chip.enabled,
            )
        }
    }
}
