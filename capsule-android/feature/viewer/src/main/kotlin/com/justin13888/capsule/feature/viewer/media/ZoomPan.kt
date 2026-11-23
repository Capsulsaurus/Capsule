package com.justin13888.capsule.feature.viewer.media

import androidx.compose.runtime.Immutable
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size

/**
 * How far into a photo the viewer is, and where.
 *
 * Pure data with pure transitions, deliberately: zoom and pan are the two gestures whose
 * arithmetic is easiest to get subtly wrong — a translation applied before its scale, a
 * clamp computed against the wrong half-extent — and every one of those mistakes is
 * invisible in a screenshot and obvious in the hand. Keeping the maths out of the gesture
 * modifier means it can be asserted directly instead of by flinging a finger at a test.
 *
 * [offset] is in pixels, measured from the centred, unzoomed position, and is always inside
 * the bounds the current [scale] allows: the photo can never be dragged so far that empty
 * space appears beside it.
 */
@Immutable
data class ZoomPan(
    /** Magnification, `1` being fit-to-frame. Never outside [MIN_SCALE]`..`[MAX_SCALE]. */
    val scale: Float = MIN_SCALE,
    /** Translation from centre, in pixels. */
    val offset: Offset = Offset.Zero,
) {
    /** Whether the photo is showing at its fit-to-frame size. */
    val isFitted: Boolean get() = scale <= MIN_SCALE

    /**
     * Applies one gesture frame: a pinch factor and a drag, in that order.
     *
     * Order matters. Scaling first and then clamping the accumulated translation is what
     * keeps a pinch-out that would push the photo off-frame from leaving a gap — zooming back
     * in on an already-panned photo pulls it back into bounds rather than stranding it.
     */
    fun transformed(
        zoomChange: Float,
        panChange: Offset,
        frame: Size,
    ): ZoomPan {
        val next = (scale * zoomChange).coerceIn(MIN_SCALE, MAX_SCALE)
        return ZoomPan(next, clampOffset(offset + panChange, next, frame))
    }

    /**
     * What a double tap does: out to the fit, or in to [DOUBLE_TAP_SCALE] about the tap.
     *
     * A double tap that always returned to the centre would be useless for the thing people
     * double-tap for — checking one face, one sign, one corner — so the tap point is carried
     * into the middle of the frame, then clamped like any other pan.
     *
     * @param tap where the user tapped, in frame coordinates with the origin at the top-left.
     */
    fun toggled(
        tap: Offset,
        frame: Size,
    ): ZoomPan {
        if (!isFitted) return ZoomPan(MIN_SCALE, Offset.Zero)
        val fromCentre = Offset(tap.x - frame.width / 2f, tap.y - frame.height / 2f)
        // Moving the tapped point to the centre means translating by its distance from the
        // centre, magnified — the point is `DOUBLE_TAP_SCALE` further out after the zoom.
        val target = -fromCentre * DOUBLE_TAP_SCALE
        return ZoomPan(DOUBLE_TAP_SCALE, clampOffset(target, DOUBLE_TAP_SCALE, frame))
    }

    /** One step of the keyboard and button zoom, which has no centroid to work from. */
    fun stepped(
        factor: Float,
        frame: Size,
    ): ZoomPan {
        val next = (scale * factor).coerceIn(MIN_SCALE, MAX_SCALE)
        return ZoomPan(next, clampOffset(offset, next, frame))
    }

    companion object {
        /** Fit to frame. Zooming out past this would show empty space around the photo. */
        const val MIN_SCALE = 1f

        /** As far in as the viewer goes; past this a screen-sized derivative is only mush. */
        const val MAX_SCALE = 6f

        /** Where a double tap lands. Close enough to read a sign, wide enough to keep context. */
        const val DOUBLE_TAP_SCALE = 2.5f

        /** The factor one press of the zoom buttons applies. */
        const val STEP_FACTOR = 1.5f

        /** The fitted, centred position. */
        val Fitted = ZoomPan()

        /**
         * Clamps a translation to what [scale] permits inside [frame].
         *
         * At scale `s` the photo overhangs the frame by `(s - 1) / 2` of the frame on each
         * side, and that overhang is exactly how far it may travel. Below scale 1 there is no
         * overhang at all, so the only legal position is centred.
         */
        fun clampOffset(
            offset: Offset,
            scale: Float,
            frame: Size,
        ): Offset {
            if (scale <= MIN_SCALE) return Offset.Zero
            val maxX = (frame.width * (scale - MIN_SCALE)) / 2f
            val maxY = (frame.height * (scale - MIN_SCALE)) / 2f
            return Offset(
                x = offset.x.coerceIn(-maxX, maxX),
                y = offset.y.coerceIn(-maxY, maxY),
            )
        }
    }
}
