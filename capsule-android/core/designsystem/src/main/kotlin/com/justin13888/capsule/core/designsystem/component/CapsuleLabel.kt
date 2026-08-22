package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/** The space Material puts between a button's leading icon and its label. */
private val IconLabelSpacing = 8.dp

/**
 * The icon-then-label content shared by every Capsule control that takes both.
 *
 * Lives in one place so the icon spacing, the single-line rule and — most importantly —
 * the decision to leave the icon undescribed cannot drift between the button, the toggle
 * and the split button. A described icon beside its own label makes a screen reader say
 * the action twice.
 */
@Composable
internal fun RowScope.CapsuleLabelWithIcon(
    label: String,
    icon: ImageVector?,
) {
    if (icon != null) {
        Icon(imageVector = icon, contentDescription = null)
        Spacer(Modifier.width(IconLabelSpacing))
    }
    Text(text = label, maxLines = 1, overflow = TextOverflow.Ellipsis)
}
