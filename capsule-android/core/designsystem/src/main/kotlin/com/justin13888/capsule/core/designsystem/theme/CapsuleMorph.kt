package com.justin13888.capsule.core.designsystem.theme

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.snap
import androidx.compose.foundation.interaction.InteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.material3.MaterialShapes
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.toPath
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.geometry.center
import androidx.compose.ui.graphics.Matrix
import androidx.compose.ui.graphics.Outline
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import androidx.graphics.shapes.Morph
import androidx.graphics.shapes.RoundedPolygon

/**
 * A [Shape] frozen at one point along a [Morph].
 *
 * Equality is by morph identity and progress so that Compose re-clips exactly when the
 * animation moves and not on every unrelated recomposition — a morphed outline is
 * rebuilt from scratch each time it is asked for, which is too expensive to do
 * speculatively on a screen holding a few hundred thumbnails.
 */
private class MorphShape(
    private val morph: Morph,
    private val progress: Float,
) : Shape {
    override fun createOutline(
        size: Size,
        layoutDirection: LayoutDirection,
        density: Density,
    ): Outline {
        val path = morph.toPath(progress = progress)
        path.transform(Matrix().apply { scale(x = size.width, y = size.height) })
        path.translate(size.center - path.getBounds().center)
        return Outline.Generic(path)
    }

    override fun equals(other: Any?) = other is MorphShape && morph === other.morph && progress == other.progress

    override fun hashCode() = 31 * morph.hashCode() + progress.hashCode()
}

/**
 * Animates 0f..1f as [active] flips, honouring [LocalReduceMotion].
 *
 * The shape morph is decoration: it tells you something the colour and the checkmark
 * already told you. When the user has asked for less motion it snaps, and the component
 * keeps its final shape rather than losing the affordance entirely.
 */
@Composable
private fun morphProgress(
    active: Boolean,
    label: String,
): Float {
    val reduceMotion = LocalReduceMotion.current
    val spec = if (reduceMotion) snap() else MaterialTheme.motionScheme.fastSpatialSpec<Float>()
    val progress by animateFloatAsState(
        targetValue = if (active) 1f else 0f,
        animationSpec = spec,
        label = label,
    )
    return progress
}

/**
 * Clips this element to a shape that morphs from [unselected] to [selected] as
 * [isSelected] flips.
 *
 * Selection is the single most common thing a photographer does in this app, and it has
 * to be readable in a grid of a thousand thumbnails at a glance. A shape change survives
 * being small, being colour-blind, and being on top of an arbitrary photograph, which is
 * more than a tint can claim.
 */
@Composable
fun Modifier.morphOnSelection(
    isSelected: Boolean,
    unselected: RoundedPolygon = MaterialShapes.Circle,
    selected: RoundedPolygon = MaterialShapes.Cookie9Sided,
): Modifier {
    val morph = remember(unselected, selected) { Morph(unselected, selected) }
    return clip(MorphShape(morph, morphProgress(isSelected, "capsule-morph-selection")))
}

/**
 * Clips this element to a shape that morphs while [interactionSource] reports a press.
 *
 * The press morph is what makes an expressive button feel like it has give. It is driven
 * from the caller's interaction source rather than an internal one so a wrapper can share
 * a single source between the visual and the click handling.
 */
@Composable
fun Modifier.morphOnPress(
    interactionSource: InteractionSource,
    resting: RoundedPolygon = MaterialShapes.Circle,
    pressed: RoundedPolygon = MaterialShapes.Cookie9Sided,
): Modifier {
    val morph = remember(resting, pressed) { Morph(resting, pressed) }
    val isPressed by interactionSource.collectIsPressedAsState()
    return clip(MorphShape(morph, morphProgress(isPressed, "capsule-morph-press")))
}
