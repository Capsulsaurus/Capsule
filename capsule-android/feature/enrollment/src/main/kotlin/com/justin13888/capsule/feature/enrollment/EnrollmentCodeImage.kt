package com.justin13888.capsule.feature.enrollment

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import kotlin.random.Random

private val CodePadding = 16.dp
private val CodeCorner = 16.dp
private const val MODULES = 33
private const val FINDER = 7

/**
 * The square code, drawn rather than encoded.
 *
 * A deliberate stand-in, and named as one: nothing in this app decodes a real symbology — the
 * scanner's decode is a fixture too — so pulling a barcode library in to produce bytes that
 * only ever travel between two fixtures would be a dependency bought with no capability. The
 * modules are a deterministic function of [payload], so the same code always draws the same
 * pattern and a screenshot golden stays stable; the finder squares are real because the shape
 * is what tells a user where to point a camera.
 *
 * Drawn black on white in both themes, because a code exists to be photographed by another
 * device's camera and a dark-theme inversion is the one thing that reliably stops that working.
 */
@Composable
internal fun EnrollmentCodeImage(
    payload: String,
    contentDescription: String,
    modifier: Modifier = Modifier,
) {
    val modules = remember(payload) { modulesFor(payload) }
    Canvas(
        modifier =
            modifier
                .aspectRatio(1f)
                .clip(RoundedCornerShape(CodeCorner))
                .background(Color.White)
                .padding(CodePadding)
                .capsuleDescription(contentDescription),
    ) {
        val cell = size.minDimension / MODULES
        for (row in 0 until MODULES) {
            for (column in 0 until MODULES) {
                if (!modules[row][column]) continue
                drawRect(
                    color = Color.Black,
                    topLeft = Offset(column * cell, row * cell),
                    size = Size(cell, cell),
                )
            }
        }
    }
}

/** The module grid for [payload]: three finder squares plus a deterministic body. */
private fun modulesFor(payload: String): Array<BooleanArray> {
    val random = Random(payload.hashCode().toLong())
    val grid = Array(MODULES) { BooleanArray(MODULES) }
    for (row in 0 until MODULES) {
        for (column in 0 until MODULES) {
            grid[row][column] = random.nextBoolean()
        }
    }
    drawFinder(grid, 0, 0)
    drawFinder(grid, 0, MODULES - FINDER)
    drawFinder(grid, MODULES - FINDER, 0)
    return grid
}

/** One finder square: a filled ring with a filled core, and a clear separator around it. */
private fun drawFinder(
    grid: Array<BooleanArray>,
    top: Int,
    left: Int,
) {
    for (row in top - 1..top + FINDER) {
        for (column in left - 1..left + FINDER) {
            if (row in grid.indices && column in grid.indices) {
                grid[row][column] = finderModule(row, column, top, left)
            }
        }
    }
}

/** Whether the module at ([row], [column]) is part of the finder square anchored at ([top], [left]). */
private fun finderModule(
    row: Int,
    column: Int,
    top: Int,
    left: Int,
): Boolean {
    val inside = row in top until top + FINDER && column in left until left + FINDER
    if (!inside) return false
    val ring = row == top || row == top + FINDER - 1 || column == left || column == left + FINDER - 1
    val core = row in top + 2..top + FINDER - 3 && column in left + 2..left + FINDER - 3
    return ring || core
}
