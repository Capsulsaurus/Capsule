package com.justin13888.capsule.core.designsystem.component

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.snap
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.designsystem.theme.LocalReduceMotion
import com.justin13888.capsule.core.designsystem.theme.morphOnSelection

private val CheckboxSize = 24.dp
private val CheckboxRingWidth = 2.dp

/**
 * A checkbox that changes shape, not just colour.
 *
 * Material's checkbox is a rounded square that fills with the primary colour. Over a
 * photograph — which is where every checkbox in this app lives, in the corner of a
 * thumbnail — a fill alone is unreadable against a light image and invisible against a
 * busy one. Morphing a circle into a nine-lobed cookie changes the silhouette, which
 * survives any background, any thumbnail size, and any colour vision.
 *
 * @param onCheckedChange `null` makes the control non-interactive, for a row that
 *   handles the toggle itself and needs the checkbox to stay out of the semantics tree.
 * @param contentDescription announced label. A bare checkbox has no visible text, so
 *   supply one unless a parent row already carries the description.
 */
@Composable
fun CapsuleCheckbox(
    checked: Boolean,
    onCheckedChange: ((Boolean) -> Unit)?,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    contentDescription: String? = null,
) {
    val reduceMotion = LocalReduceMotion.current
    val spec = if (reduceMotion) snap() else MaterialTheme.motionScheme.defaultEffectsSpec<Color>()
    val selected = CapsuleTheme.colors.selected
    val ring by animateColorAsState(
        targetValue = if (checked) selected.color else MaterialTheme.colorScheme.outline,
        animationSpec = spec,
        label = "capsule-checkbox-ring",
    )
    val fill by animateColorAsState(
        targetValue = if (checked) selected.color else MaterialTheme.colorScheme.surface,
        animationSpec = spec,
        label = "capsule-checkbox-fill",
    )

    val interactive =
        if (onCheckedChange == null) {
            Modifier
        } else {
            Modifier.toggleable(
                value = checked,
                enabled = enabled,
                role = Role.Checkbox,
                onValueChange = onCheckedChange,
            )
        }

    Box(
        modifier =
            modifier
                .capsuleTouchTarget()
                .then(interactive)
                .capsuleDescription(contentDescription),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier =
                Modifier
                    .size(CheckboxSize)
                    .morphOnSelection(checked)
                    .background(ring),
            contentAlignment = Alignment.Center,
        ) {
            Box(
                modifier =
                    Modifier
                        .padding(CheckboxRingWidth)
                        .size(CheckboxSize - CheckboxRingWidth * 2)
                        .morphOnSelection(checked)
                        .background(fill),
            )
            if (checked) {
                Icon(
                    imageVector = Icons.Filled.Check,
                    contentDescription = null,
                    tint = selected.onColor,
                )
            }
        }
    }
}
