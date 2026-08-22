package com.justin13888.capsule.core.ui.gallery

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import kotlin.math.abs

/** How far a pinch must travel before it takes the pointer stream away from scrolling. */
private const val PINCH_SLOP = 0.06f

/** The visual scale a pinch is allowed to reach before it stops tracking the fingers. */
private const val MIN_GESTURE_SCALE = 0.35f
private const val MAX_GESTURE_SCALE = 3.0f

/**
 * Which rung of the zoom ladder the gallery is on, and how far a pinch has travelled from
 * it.
 *
 * The gesture is continuous and the layout is not, and this is where the two are
 * reconciled. During the pinch nothing is re-planned: [gestureScale] scales the already
 * composed grid through a `graphicsLayer`, which is a GPU transform of what is already on
 * screen and costs nothing per frame. On release the scale is thrown away and the nearest
 * rung is chosen, which is when the caller re-plans — once, not sixty times a second.
 *
 * [previousLevelIndex] exists for double-tap. Toggling between the last two rungs is the
 * gesture people actually use to check a frame and come back, and "the last two" is a
 * fact about this session, not something derivable from the ladder.
 */
@Stable
class GalleryZoomState internal constructor(
    levels: GalleryDensityLevels,
    initialIndex: Int,
    initialPreviousIndex: Int,
) {
    /**
     * The ladder in force.
     *
     * Mutable because the ladder depends on window width, and a window can be resized
     * without the gallery being recreated — a folding phone opening mid-scroll.
     */
    var levels: GalleryDensityLevels by mutableStateOf(levels)

    /** The current rung, densest is 0. */
    var levelIndex: Int by mutableIntStateOf(levels.coerce(initialIndex))
        private set

    /** The rung a double tap returns to. */
    var previousLevelIndex: Int by mutableIntStateOf(levels.coerce(initialPreviousIndex))
        private set

    /** The current rung. */
    val level: GalleryDensityLevel get() = levels[levelIndex]

    /** The visual scale to apply while a pinch is in flight; 1 when it is not. */
    var gestureScale: Float by mutableFloatStateOf(1f)
        private set

    /** Whether a pinch is currently driving [gestureScale]. */
    var isZooming: Boolean by mutableStateOf(false)
        private set

    /** Jumps to a rung, remembering the one being left so double tap can return to it. */
    fun setLevel(index: Int) {
        val next = levels.coerce(index)
        if (next == levelIndex) return
        previousLevelIndex = levelIndex
        levelIndex = next
    }

    /** One rung sparser: fewer, larger photos. */
    fun zoomIn() = setLevel(levelIndex + 1)

    /** One rung denser: more, smaller photos. */
    fun zoomOut() = setLevel(levelIndex - 1)

    /** Whether [zoomIn] would change anything. */
    val canZoomIn: Boolean get() = levelIndex < levels.size - 1

    /** Whether [zoomOut] would change anything. */
    val canZoomOut: Boolean get() = levelIndex > 0

    /** Swaps between the current rung and the one before it. Double tap lands here. */
    fun toggle() = setLevel(previousLevelIndex)

    internal fun onGestureStart() {
        isZooming = true
        gestureScale = 1f
    }

    internal fun onPinch(scale: Float) {
        gestureScale = scale.coerceIn(MIN_GESTURE_SCALE, MAX_GESTURE_SCALE)
    }

    /**
     * Ends the pinch and snaps to the rung the gesture asked for.
     *
     * A scale of `s` means "make each photo `s` times bigger", which is the same as "fit
     * `across / s` of them across", so the wanted across-count is what gets matched
     * against the ladder — in log space, see [GalleryDensityLevels.nearestTo].
     */
    internal fun onGestureEnd() {
        val wanted = level.assetsAcross / gestureScale
        val index = levels.nearestTo(wanted)
        gestureScale = 1f
        isZooming = false
        setLevel(index)
    }

    companion object {
        /** Saves both rungs; the ladder itself is re-derived from the restored window width. */
        fun saver(levels: GalleryDensityLevels): Saver<GalleryZoomState, List<Int>> =
            Saver(
                save = { listOf(it.levelIndex, it.previousLevelIndex) },
                restore = { saved ->
                    GalleryZoomState(
                        levels = levels,
                        initialIndex = saved.getOrElse(0) { levels.defaultIndex },
                        initialPreviousIndex = saved.getOrElse(1) { levels.defaultIndex },
                    )
                },
            )
    }
}

/**
 * A [GalleryZoomState] that survives recomposition and configuration change.
 *
 * [levels] is applied on every recomposition rather than only at creation, so swapping the
 * ladder when a window is resized keeps the rung index meaningful instead of leaving the
 * state pointing past the end of a shorter ladder.
 */
@Composable
fun rememberGalleryZoomState(levels: GalleryDensityLevels): GalleryZoomState {
    val state =
        rememberSaveable(saver = GalleryZoomState.saver(levels)) {
            GalleryZoomState(
                levels = levels,
                initialIndex = levels.defaultIndex,
                initialPreviousIndex = levels.defaultIndex,
            )
        }
    if (state.levels !== levels) state.levels = levels
    return state
}

/**
 * Pinch to scale the grid continuously, release to snap to the nearest density rung, and
 * double tap to toggle between the last two.
 *
 * The anchor is captured at the moment the level changes and not a frame later. By the
 * time the caller has re-planned, the old plan is gone; asking it afterwards which photo
 * was under the fingers is not possible, which is why [GalleryState.captureAnchor] is
 * called here rather than by whoever re-plans.
 *
 * The pinch loop is hand-rolled rather than `detectTransformGestures` because that helper
 * has no end callback, and the end is the only moment that matters — it is where the
 * gesture stops being a GPU transform and becomes a layout.
 *
 * @param enabled `false` leaves the pointer stream untouched, for a fixed-density gallery.
 */
fun Modifier.galleryZoom(
    state: GalleryState,
    zoom: GalleryZoomState,
    enabled: Boolean = true,
): Modifier {
    if (!enabled) return this
    return this
        .pointerInput(zoom, state) {
            awaitEachGesture {
                awaitFirstDown(requireUnconsumed = false)
                var accumulated = 1f
                var engaged = false
                var pressed: Boolean
                do {
                    val event = awaitPointerEvent()
                    if (event.changes.count { it.pressed } >= 2) {
                        val step = event.calculateZoom()
                        if (step != 0f) accumulated *= step
                        if (!engaged && abs(accumulated - 1f) >= PINCH_SLOP) {
                            engaged = true
                            zoom.onGestureStart()
                        }
                        if (engaged) {
                            zoom.onPinch(accumulated)
                            event.changes.forEach { if (it.pressed) it.consume() }
                        }
                    }
                    pressed = event.changes.any { it.pressed }
                } while (pressed)
                if (engaged) {
                    state.captureAnchor()
                    zoom.onGestureEnd()
                }
            }
        }.pointerInput(zoom, state) {
            detectTapGestures(
                onDoubleTap = {
                    state.captureAnchor()
                    zoom.toggle()
                },
            )
        }
}
