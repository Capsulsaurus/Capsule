package com.justin13888.capsule.core.mock.image

import com.justin13888.capsule.core.model.RgbColor

/**
 * Renders a [ChromaGrid] as a smooth mesh gradient.
 *
 * Bicubic rather than bilinear interpolation, because the difference is visible at the size a
 * placeholder is actually drawn: bilinear over a 3×3 control grid leaves creases along the
 * cell boundaries, and a grid of creased tiles reads as a rendering bug rather than as a
 * blurred photo. Catmull-Rom costs four taps per axis and removes them entirely.
 *
 * The renderer is pure arithmetic over an [IntArray], so the same grid produces byte-identical
 * pixels on every device — which is what lets a screenshot golden of a loading gallery exist
 * at all.
 */
object MeshGradient {
    /** Renders [grid] into a [width] × [height] image. */
    fun render(
        grid: ChromaGrid,
        width: Int,
        height: Int,
    ): ArgbImage {
        val pixels = IntArray(width * height)
        val lastColumn = (ChromaGrid.SIDE - 1).toDouble()
        for (y in 0 until height) {
            val gridY = (y + HALF) / height * lastColumn
            for (x in 0 until width) {
                val gridX = (x + HALF) / width * lastColumn
                pixels[y * width + x] = sample(grid, gridX, gridY)
            }
        }
        return ArgbImage(width, height, pixels)
    }

    /** The interpolated colour at continuous grid coordinates [gridX], [gridY]. */
    private fun sample(
        grid: ChromaGrid,
        gridX: Double,
        gridY: Double,
    ): Int {
        val columnBase = kotlin.math.floor(gridX).toInt()
        val rowBase = kotlin.math.floor(gridY).toInt()
        val withinColumn = gridX - columnBase
        val withinRow = gridY - rowBase
        var red = 0.0
        var green = 0.0
        var blue = 0.0
        for (rowTap in -1..2) {
            val rowWeight = catmullRom(withinRow - rowTap)
            if (rowWeight == 0.0) continue
            for (columnTap in -1..2) {
                val weight = rowWeight * catmullRom(withinColumn - columnTap)
                if (weight == 0.0) continue
                val cell = clampedCell(grid, columnBase + columnTap, rowBase + rowTap)
                red += cell.red * weight
                green += cell.green * weight
                blue += cell.blue * weight
            }
        }
        return ArgbImage.pack(red.toInt(), green.toInt(), blue.toInt())
    }

    /** The cell at [column], [row], with out-of-range taps clamped to the edge. */
    private fun clampedCell(
        grid: ChromaGrid,
        column: Int,
        row: Int,
    ): RgbColor =
        grid.cell(
            column = column.coerceIn(0, ChromaGrid.SIDE - 1),
            row = row.coerceIn(0, ChromaGrid.SIDE - 1),
        )

    /**
     * The Catmull-Rom kernel, which sums to one over any four consecutive taps.
     *
     * Written out rather than pulled from a library: it is eight lines, and a dependency here
     * would put a third-party version bump in the path of every screenshot golden.
     */
    private fun catmullRom(distance: Double): Double {
        val d = kotlin.math.abs(distance)
        return when {
            d < 1.0 -> ((THREE_HALVES * d - FIVE_HALVES) * d * d) + 1.0
            d < 2.0 -> (((-HALF * d + FIVE_HALVES) * d - FOUR) * d) + TWO
            else -> 0.0
        }
    }

    private const val HALF = 0.5
    private const val TWO = 2.0
    private const val FOUR = 4.0
    private const val THREE_HALVES = 1.5
    private const val FIVE_HALVES = 2.5
}
