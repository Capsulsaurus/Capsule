package com.justin13888.capsule.core.ui.gallery

import androidx.compose.runtime.Immutable
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlin.math.abs
import kotlin.math.ln

/**
 * One rung of the zoom ladder, named by how many square thumbnails would fit across the
 * viewport at that rung.
 *
 * Levels are expressed as a count rather than as a height in dp because that is the thing
 * the user actually perceives and the only thing that stays honest across screen sizes: a
 * 120dp row is a comfortable three-across on a phone and an absurd nine-across on a
 * tablet. The height is derived from the count and the viewport at solve time.
 *
 * @param assetsAcross how many square assets a row would hold. Real rows hold more or
 *   fewer depending on the aspect ratios the solver is handed; this fixes the *height*,
 *   not the count.
 */
@Immutable
data class GalleryDensityLevel(
    val assetsAcross: Int,
) {
    init {
        require(assetsAcross >= 1) { "a density level needs at least one asset across, got $assetsAcross" }
    }

    /**
     * The [GalleryMetrics.targetRowHeight] this level asks for.
     *
     * Subtracts the inter-asset gaps first, so at one-across the asset really does fill
     * the viewport rather than overflowing it by the spacing.
     */
    fun targetRowHeight(
        viewportWidth: Float,
        spacing: Float,
    ): Float {
        val available = viewportWidth - spacing * (assetsAcross - 1)
        return (available / assetsAcross).coerceAtLeast(1f)
    }
}

/**
 * The zoom ladder a gallery snaps to, densest rung first.
 *
 * A continuous pinch is a pleasant gesture and a terrible layout input: every intermediate
 * scale is a re-plan, and a library settled at 4.37-across looks like a rendering bug. So
 * the pinch drives a *visual* scale and the release picks the nearest rung — which is also
 * what makes the gesture reversible, because the rungs are the only states that exist.
 */
@Immutable
class GalleryDensityLevels(
    /** The rungs, ordered densest (most across) to sparsest. */
    val levels: List<GalleryDensityLevel>,
) {
    init {
        require(levels.isNotEmpty()) { "a zoom ladder needs at least one level" }
        for (i in 1 until levels.size) {
            require(levels[i].assetsAcross < levels[i - 1].assetsAcross) {
                "levels must descend; ${levels[i - 1].assetsAcross} then ${levels[i].assetsAcross}"
            }
        }
    }

    /** How many rungs the ladder has. */
    val size: Int get() = levels.size

    /** The rung at [index]. */
    operator fun get(index: Int): GalleryDensityLevel = levels[index.coerceIn(0, levels.lastIndex)]

    /** [index] clamped into the ladder. */
    fun coerce(index: Int): Int = index.coerceIn(0, levels.lastIndex)

    /**
     * The rung closest to [assetsAcross], compared in log space.
     *
     * Log space because the ladder is geometric: 7-to-5 and 2-to-1 are both "one step",
     * and a linear nearest would make every release near the sparse end snap to 1.
     */
    fun nearestTo(assetsAcross: Float): Int {
        val wanted = assetsAcross.coerceAtLeast(0.01f)
        var best = 0
        var bestDistance = Float.MAX_VALUE
        levels.forEachIndexed { index, level ->
            val distance = abs(ln(wanted / level.assetsAcross))
            if (distance < bestDistance) {
                bestDistance = distance
                best = index
            }
        }
        return best
    }

    /**
     * The rung a library opens at.
     *
     * Three across: dense enough that a day of shooting is one screen, loose enough that
     * a face is recognisable without opening anything.
     */
    val defaultIndex: Int get() = nearestTo(DEFAULT_ASSETS_ACROSS.toFloat())

    companion object {
        /** Below this window width a hand is holding the device and the ladder is short. */
        val CompactWidthMax: Dp = 600.dp

        private const val DEFAULT_ASSETS_ACROSS = 3

        /** The phone ladder: contact sheet, dense, browse, pair, single. */
        val Compact: GalleryDensityLevels = of(7, 5, 3, 2, 1)

        /**
         * The tablet and desktop ladder.
         *
         * The same five rungs with two coarser ones on top: a 1000dp window at
         * seven-across is already a comfortable browse size, so the phone's densest rung
         * is the wide window's *middle*, and without the extra steps a pinch on a tablet
         * would run out of ladder before it ran out of screen.
         */
        val Expanded: GalleryDensityLevels = of(12, 9, 7, 5, 3, 2, 1)

        /** Builds a ladder from a descending list of across-counts. */
        fun of(vararg across: Int): GalleryDensityLevels = GalleryDensityLevels(across.map(::GalleryDensityLevel))

        /** The ladder for a window of [width]. */
        fun forWidth(width: Dp): GalleryDensityLevels = if (width < CompactWidthMax) Compact else Expanded
    }
}
