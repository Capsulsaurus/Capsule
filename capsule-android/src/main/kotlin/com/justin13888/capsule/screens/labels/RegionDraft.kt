package com.justin13888.capsule.screens.labels

import com.justin13888.capsule.screens.domain.RegionShape
import kotlin.math.abs

/**
 * A region being drawn over an asset's preview, in fractions of the image over its
 * display orientation.
 *
 * Every edit keeps the draft inside the shape contract the core validates
 * ([RegionShape.validate]), so [shape] is always writable: a drag past an edge stops at
 * the edge, and a resize never shrinks below [MINIMUM_EXTENT] or grows past the image. A
 * circle's radius is a fraction of the image's **shorter** side, and since the shape does
 * not record which side that is, its centre keeps the radius from every edge on both
 * axes.
 *
 * Gestures move it by any amount; the TalkBack adjust actions move it by [STEP], the
 * TUI's arrow-key step, so a TalkBack user draws exactly the regions a keyboard user
 * does. The same geometry as the Apple client's `RegionDraft`.
 */
sealed interface RegionDraft {
    /** Which kind this draft is. */
    val kind: Kind

    /** A circle: centre (of width across, of height down) and radius (of the shorter side). */
    data class Circle(
        val centerX: Double,
        val centerY: Double,
        val radius: Double,
    ) : RegionDraft {
        override val kind: Kind get() = Kind.CIRCLE
    }

    /** A rectangle: top-left corner, width and height. */
    data class Rect(
        val left: Double,
        val top: Double,
        val width: Double,
        val height: Double,
    ) : RegionDraft {
        override val kind: Kind get() = Kind.RECT
    }

    /** What a new draft is. */
    enum class Kind {
        CIRCLE,
        RECT,
    }

    /** Move by ([dx], [dy]) fractions, keeping the whole shape inside the image. */
    fun moved(
        dx: Double,
        dy: Double,
    ): RegionDraft =
        when (this) {
            is Circle -> Circle(clamp(centerX + dx, radius, 1 - radius), clamp(centerY + dy, radius, 1 - radius), radius)
            is Rect -> Rect(clamp(left + dx, 0.0, 1 - width), clamp(top + dy, 0.0, 1 - height), width, height)
        }

    /**
     * Resize by ([dw], [dh]) fractions, never below [MINIMUM_EXTENT] and never past the
     * image. A circle's radius changes by the larger of the two, and grows only until it
     * meets the nearest edge on either axis.
     */
    fun resized(
        dw: Double,
        dh: Double,
    ): RegionDraft =
        when (this) {
            is Circle -> {
                val change = if (abs(dw) >= abs(dh)) dw else dh
                val room = maxOf(minOf(centerX, 1 - centerX, minOf(centerY, 1 - centerY)), MINIMUM_EXTENT)
                Circle(centerX, centerY, clamp(radius + change, MINIMUM_EXTENT, room))
            }
            is Rect ->
                Rect(left, top, clamp(width + dw, MINIMUM_EXTENT, 1 - left), clamp(height + dh, MINIMUM_EXTENT, 1 - top))
        }

    /** Scale about the shape's own centre by [factor] (a pinch), within the same bounds as [resized]. */
    fun scaled(factor: Double): RegionDraft {
        if (!factor.isFinite() || factor <= 0) return this
        return when (this) {
            is Circle -> resized(radius * (factor - 1), 0.0)
            is Rect -> {
                val newWidth = clamp(width * factor, MINIMUM_EXTENT, 1.0)
                val newHeight = clamp(height * factor, MINIMUM_EXTENT, 1.0)
                val centerX = left + width / 2
                val centerY = top + height / 2
                Rect(
                    clamp(centerX - newWidth / 2, 0.0, 1 - newWidth),
                    clamp(centerY - newHeight / 2, 0.0, 1 - newHeight),
                    newWidth,
                    newHeight,
                )
            }
        }
    }

    /**
     * The shape to write, in the core's single precision.
     *
     * Two fractions that meet an edge exactly can round past `1` in `Float`, and the core
     * refuses a shape past the edge, so the extent gives up the last representable step
     * until the shape fits.
     */
    val shape: RegionShape
        get() =
            when (this) {
                is Circle -> {
                    val middleX = centerX.toFloat()
                    val middleY = centerY.toFloat()
                    var fitted = radius.toFloat()
                    while (overflowsFrame(middleX, middleY, fitted)) {
                        fitted = Math.nextDown(fitted)
                    }
                    RegionShape.Circle(middleX, middleY, fitted)
                }
                is Rect -> {
                    val edgeX = left.toFloat()
                    val edgeY = top.toFloat()
                    RegionShape.Rect(edgeX, edgeY, fit(edgeX, width.toFloat()), fit(edgeY, height.toFloat()))
                }
            }

    companion object {
        /** How far one accessibility adjustment moves or resizes a draft. */
        const val STEP = 0.02

        /** The smallest radius, width or height a draft shrinks to. */
        const val MINIMUM_EXTENT = 0.02

        /** A new draft of [kind] in the middle of the image. */
        fun of(kind: Kind): RegionDraft =
            when (kind) {
                Kind.CIRCLE -> Circle(0.5, 0.5, 0.16)
                Kind.RECT -> Rect(0.34, 0.34, 0.32, 0.32)
            }

        private fun clamp(
            value: Double,
            low: Double,
            high: Double,
        ): Double = minOf(maxOf(value, low), maxOf(low, high))

        private fun fit(
            start: Float,
            extent: Float,
        ): Float {
            var fitted = extent
            while (start + fitted > 1f) {
                fitted = Math.nextDown(fitted)
            }
            return fitted
        }

        /** Whether a circle of [radius] centred on ([x], [y]) crosses an edge of the unit frame. */
        private fun overflowsFrame(
            x: Float,
            y: Float,
            radius: Float,
        ): Boolean = x < radius || y < radius || x + radius > 1f || y + radius > 1f
    }
}
