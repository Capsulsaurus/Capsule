package com.justin13888.capsule.core.ui.gallery

import kotlin.math.ln
import kotlin.math.min

/**
 * Packs assets into justified rows.
 *
 * A row of `n` assets drawn at a common height `h` occupies
 * `h * (a1 + a2 + … + an) + spacing * (n - 1)` horizontally, where each `a` is that
 * asset's aspect ratio. Fixing that to the viewport width and solving for `h` gives the
 * height at which the row fills the viewport exactly:
 *
 * ```
 * h = (viewportWidth - spacing * (n - 1)) / sumOfAspects
 * ```
 *
 * Choosing where to break rows is a **global** decision, not a local one. A greedy pass
 * with one-item lookahead gets a real photo library wrong in a way that is easy to see:
 * four portraits followed by a panorama will happily fold the panorama into the portrait
 * row, dragging every asset in it down to a third of the target height. The fix is not a
 * better heuristic but a different algorithm — this solves the row breaks for a whole
 * section by dynamic programming, minimising total badness rather than accepting the
 * first tolerable row.
 *
 * Badness is `ln(height / target)²`, which is scale-invariant: a row twice the target
 * height scores exactly as badly as one half of it. Absolute difference does not have
 * that property and biases the solver toward short rows.
 *
 * The trailing row is exempt. A section rarely ends on an exact fill, and forcing the
 * remainder to justify would stretch a lone asset across the viewport; it is drawn at its
 * natural height instead, capped, and left ragged — which reads as the end of a section.
 *
 * Cost is `O(n · MAX_ROW_RUN)` per section, and sections are day buckets, so a
 * million-asset library solves in a few milliseconds. Every function here is pure: same
 * inputs, same rows, which is what lets the gallery re-solve on every width and zoom
 * change and lets tests pin the output against fixtures.
 */
object RowSolver {
    /**
     * The most assets any row may hold.
     *
     * This bounds the dynamic program, but it also encodes a display judgement: past
     * about a dozen assets a row is too dense to read at any plausible viewport width.
     */
    const val MAX_ROW_RUN: Int = 12

    /**
     * The most a trailing row may stretch beyond the target height.
     *
     * A final row holding one narrow asset would otherwise justify to an absurd height.
     */
    private const val LAST_ROW_MAX_STRETCH = 1.5f

    /**
     * Pack `aspects[from until until]` into justified rows.
     *
     * @param aspects aspect ratio (width / height) per asset, indexed globally.
     * @param from first asset to lay out, inclusive.
     * @param until one past the last asset to lay out.
     * @param targetHeight the height rows aim for, in pixels.
     * @param viewportWidth the width rows must fill, in pixels.
     * @param spacing the gap between adjacent assets in a row, in pixels.
     */
    @Suppress("LongParameterList")
    fun solve(
        aspects: FloatArray,
        from: Int,
        until: Int,
        targetHeight: Float,
        viewportWidth: Float,
        spacing: Float,
    ): List<JustifiedRow> {
        require(from in 0..until) { "invalid range [$from, $until)" }
        require(until <= aspects.size) { "range [$from, $until) exceeds ${aspects.size} aspects" }
        require(targetHeight > 0f) { "targetHeight must be positive, got $targetHeight" }
        require(viewportWidth > 0f) { "viewportWidth must be positive, got $viewportWidth" }
        require(spacing >= 0f) { "spacing cannot be negative, got $spacing" }

        val count = until - from
        if (count == 0) return emptyList()

        // cost[i] is the best total badness for laying out assets [from + i, until).
        // best[i] records how many assets the first row of that solution holds, so the
        // chosen breaks can be walked back out afterwards.
        val cost = DoubleArray(count + 1)
        val best = IntArray(count + 1)
        cost[count] = 0.0

        for (i in count - 1 downTo 0) {
            var bestCost = Double.MAX_VALUE
            var bestRun = 1
            var sumOfAspects = 0f
            val maxRun = min(MAX_ROW_RUN, count - i)

            for (run in 1..maxRun) {
                sumOfAspects += aspects[from + i + run - 1]
                val height = heightOf(sumOfAspects, run, viewportWidth, spacing)
                val reachesEnd = i + run == count
                // A trailing row is exempt only when it is genuinely *underfull* — it
                // has too little content to reach the target height across the viewport,
                // so leaving it ragged is the right answer and costs nothing. An
                // overfull trailing row is charged like any other; without that, one row
                // swallowing the whole section would always score zero and win.
                val rowCost =
                    if (reachesEnd && height >= targetHeight) {
                        0.0
                    } else {
                        badnessOf(height, targetHeight)
                    }
                val total = rowCost + cost[i + run]
                if (total < bestCost) {
                    bestCost = total
                    bestRun = run
                }
                // Rows only get shorter as assets are added, so once a row has fallen
                // well below the target every longer run is worse. Stop early.
                if (!reachesEnd && height < targetHeight * MIN_USEFUL_HEIGHT_FRACTION) break
            }

            cost[i] = bestCost
            best[i] = bestRun
        }

        val rows = ArrayList<JustifiedRow>()
        var i = 0
        while (i < count) {
            val run = best[i]
            var sumOfAspects = 0f
            for (k in 0 until run) sumOfAspects += aspects[from + i + k]
            val height = heightOf(sumOfAspects, run, viewportWidth, spacing)
            val reachesEnd = i + run == count
            val drawnHeight = if (reachesEnd) min(height, targetHeight * LAST_ROW_MAX_STRETCH) else height
            rows += JustifiedRow(from + i, from + i + run, drawnHeight)
            i += run
        }
        return rows
    }

    /**
     * Scale-invariant row badness: a row twice the target height is exactly as wrong as
     * one half of it, which absolute difference would not capture.
     */
    private fun badnessOf(
        height: Float,
        targetHeight: Float,
    ): Double {
        val ratio = ln((height / targetHeight).toDouble())
        return ratio * ratio
    }

    /** The height at which `count` assets of total aspect [sumOfAspects] fill the width. */
    private fun heightOf(
        sumOfAspects: Float,
        count: Int,
        viewportWidth: Float,
        spacing: Float,
    ): Float {
        val available = viewportWidth - spacing * (count - 1)
        return if (available <= 0f || sumOfAspects <= 0f) Float.MAX_VALUE else available / sumOfAspects
    }

    /**
     * The width asset [index] is drawn at inside [row] — its aspect ratio scaled to the
     * row's common height. Callers place assets left to right, adding the row spacing
     * between them.
     */
    fun widthOf(
        aspects: FloatArray,
        row: JustifiedRow,
        index: Int,
    ): Float {
        require(index in row.from until row.until) { "asset $index is not in row $row" }
        return aspects[index] * row.height
    }

    /** Below this fraction of the target, adding more assets can only make a row worse. */
    private const val MIN_USEFUL_HEIGHT_FRACTION = 0.4f
}
