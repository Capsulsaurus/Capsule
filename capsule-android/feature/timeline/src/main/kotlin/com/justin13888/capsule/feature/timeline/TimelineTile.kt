package com.justin13888.capsule.feature.timeline

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.waitForUpOrCancellation
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.runtime.Composable
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import com.justin13888.capsule.core.ui.gallery.GalleryCellChrome

/**
 * One photograph in the grid.
 *
 * The picture itself is the asset's own dominant colour rather than a decoded derivative.
 * That is the ladder's floor — the LQIP rides inside the encrypted metadata blob and costs
 * no request at all — and it is genuinely what a tile shows before anything else arrives, so
 * drawing it is not a placeholder for a placeholder. A decoding path replaces the fill here
 * and nothing else on this screen moves.
 *
 * A degraded tile is drawn dimmer as well as badged: the badge says *why*, and the dimming
 * says *at a glance*, which is what a grid of two hundred thumbnails needs.
 *
 * The tap detector is deliberately hand-rolled and consumes nothing. The gallery's own gesture
 * stack sits above this cell and owns the long press that starts a drag-select and the pinch
 * that re-plans the grid; a `clickable` here would consume the pointer down and silently take
 * both of those away.
 *
 * It publishes no semantics of its own either. The gallery already exposes one labelled,
 * grid-positioned node per photograph, and a second node here — actionable but with nothing
 * of its own to say — would give a screen reader two stops per tile and fail an
 * unlabelled-control check into the bargain. A screen reader's activation lands as a tap
 * inside those bounds, which is exactly what this detector is listening for.
 *
 * @param cell what to draw, or `null` while this tile's window is still being read.
 * @param onOpen invoked on a tap that was not stolen by a gesture above.
 */
@Composable
fun TimelineTile(
    cell: TimelineCell?,
    onOpen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val open = rememberUpdatedState(onOpen)
    Box(
        modifier =
            modifier
                .fillMaxSize()
                .background(cell.tileBrush())
                .pointerInput(cell?.assetId) {
                    awaitEachGesture {
                        awaitFirstDown(requireUnconsumed = false)
                        val up = waitForUpOrCancellation()
                        if (up != null && !up.isConsumed) open.value()
                    }
                },
    ) {
        if (cell != null && !cell.markers.isEmpty) {
            // The shared chrome draws its markers with no ground of its own, because the
            // gallery cannot know what is behind them. The tile can: a wash along the bottom
            // edge is what gives an amber upload mark and a white heart something to be read
            // against on a pale photograph, and it costs one gradient rather than a scrim per
            // marker.
            Box(
                modifier =
                    Modifier
                        .align(Alignment.BottomCenter)
                        .fillMaxWidth()
                        .fillMaxHeight(MARKER_SCRIM_FRACTION)
                        .background(MarkerScrim),
            )
        }
        if (cell != null) GalleryCellChrome(markers = cell.markers)
    }
}

/**
 * The fill for one tile.
 *
 * A vertical wash rather than a flat fill, because a grid of flat rectangles reads as a
 * loading skeleton and a grid of gently graded ones reads as photographs that have not
 * sharpened yet — which is the honest description of what an LQIP is.
 */
@Composable
private fun TimelineCell?.tileBrush(): Brush {
    if (this == null) return Brush.verticalGradient(listOf(UnreadTop, UnreadBottom))
    val dim = if (degradation != null) DEGRADED_ALPHA else 1f
    return Brush.verticalGradient(
        listOf(
            placeholder.copy(alpha = dim),
            placeholder.darkened().copy(alpha = dim),
        ),
    )
}

/** The bottom of a tile's wash — the same colour, a shade down. */
private fun Color.darkened(): Color =
    Color(
        red = red * SHADE,
        green = green * SHADE,
        blue = blue * SHADE,
        alpha = alpha,
    )

/** The wash that gives the corner markers a ground on an arbitrary photograph. */
private val MarkerScrim =
    Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.7f)))

/** How much of the tile's height the marker wash covers. */
private const val MARKER_SCRIM_FRACTION = 0.45f

/** What a tile whose window has not been read yet fills with. */
private val UnreadTop = Color(0xFF2A2A2E)
private val UnreadBottom = Color(0xFF1E1E22)

private const val SHADE = 0.78f

/** How far a degraded tile is knocked back, so the badge has something to explain. */
private const val DEGRADED_ALPHA = 0.55f
