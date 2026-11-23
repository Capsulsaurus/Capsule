package com.justin13888.capsule.feature.viewer.media

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.toSize

/**
 * The full-bleed photo, with pinch, drag and double tap on it.
 *
 * Gesture handling is split across two `pointerInput` blocks on purpose: transform gestures
 * and tap gestures each consume the pointers they recognise, and a single detector that tried
 * to arbitrate between "two fingers moving" and "one finger tapping twice" would drop one of
 * them. The arithmetic they feed is [ZoomPan]'s, not theirs, so the clamping is asserted
 * directly rather than by flinging a synthetic finger.
 *
 * The measured frame is hoisted rather than kept here because the buttons that zoom without
 * a centroid live in the control strip — which in tabletop posture is on the other side of a
 * physical hinge — and they need the same bounds to clamp against.
 *
 * @param render what to draw.
 * @param zoom the current magnification and translation.
 * @param frame the surface's measured size, used to clamp translation.
 * @param onFrameChange reports a new measured size.
 * @param onZoomChange receives every new position, already clamped to the frame.
 * @param contentDescription the announced description of the photo itself.
 */
@Suppress("LongParameterList")
@Composable
fun AssetSurface(
    render: AssetRender,
    zoom: ZoomPan,
    frame: Size,
    onFrameChange: (Size) -> Unit,
    onZoomChange: (ZoomPan) -> Unit,
    contentDescription: String,
    modifier: Modifier = Modifier,
) {
    val fill = render.brush(fallback = MaterialTheme.colorScheme.surfaceContainerHighest)
    Box(
        modifier =
            modifier
                .fillMaxSize()
                .onSizeChanged { onFrameChange(it.toSize()) }
                .pointerInput(render) {
                    detectTransformGestures { _, pan, zoomChange, _ ->
                        onZoomChange(zoom.transformed(zoomChange, pan, frame))
                    }
                }.pointerInput(render) {
                    detectTapGestures(onDoubleTap = { tap -> onZoomChange(zoom.toggled(tap, frame)) })
                }.semantics { this.contentDescription = contentDescription },
        contentAlignment = Alignment.Center,
    ) {
        Canvas(
            modifier =
                Modifier
                    .aspectRatio(render.aspectRatio)
                    .fillMaxSize()
                    .graphicsLayer {
                        scaleX = zoom.scale
                        scaleY = zoom.scale
                        translationX = zoom.offset.x
                        translationY = zoom.offset.y
                    },
        ) {
            drawRect(brush = fill)
        }
    }
}

/**
 * The fill for one asset.
 *
 * A flat wash for an undecodable file and a soft gradient for everything else. The
 * distinction is deliberate and visible from across a room: a photograph the device merely
 * has not fetched at full size still looks like a photograph, while one it cannot decode at
 * all should not pretend to be an image it is not showing.
 */
private fun AssetRender.brush(fallback: Color): Brush {
    val base = dominantColor ?: fallback
    if (isUnreadable) return Brush.linearGradient(listOf(base, base))
    return Brush.linearGradient(
        listOf(
            base.lightened(HIGHLIGHT_SHIFT),
            base,
            base.darkened(SHADOW_SHIFT),
        ),
    )
}

/** Moves a colour toward white by [amount], leaving alpha alone. */
private fun Color.lightened(amount: Float): Color =
    Color(
        red = red + (1f - red) * amount,
        green = green + (1f - green) * amount,
        blue = blue + (1f - blue) * amount,
        alpha = alpha,
    )

/** Moves a colour toward black by [amount], leaving alpha alone. */
private fun Color.darkened(amount: Float): Color =
    Color(red = red * (1f - amount), green = green * (1f - amount), blue = blue * (1f - amount), alpha = alpha)

/** How far the top of the gradient lifts toward white. */
private const val HIGHLIGHT_SHIFT = 0.28f

/** How far the bottom of the gradient falls toward black. */
private const val SHADOW_SHIFT = 0.34f
