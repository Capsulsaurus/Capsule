package com.justin13888.capsule.core.ui.gallery

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import kotlin.math.abs
import kotlin.random.Random

/**
 * The gallery re-solves the entire library on every width or zoom change and trusts the
 * result to be exact, so the solver is pinned down here rather than eyeballed on a device.
 */
class RowSolverTest {
    private val width = 1000f
    private val spacing = 4f
    private val target = 200f

    /** Every row a solve produces must fill the viewport, or be the capped trailing row. */
    private fun assertRowsFillWidth(aspects: FloatArray, rows: List<JustifiedRow>) {
        rows.dropLast(1).forEach { row ->
            val content = (row.from until row.until).sumOf { RowSolver.widthOf(aspects, row, it).toDouble() }
            val total = content + spacing * (row.count - 1)
            assertTrue(
                abs(total - width) < 0.5,
                "row $row spans $total, expected $width",
            )
        }
    }

    /** Rows must tile the range exactly: no gaps, no overlaps, nothing dropped. */
    private fun assertRowsTile(rows: List<JustifiedRow>, from: Int, until: Int) {
        if (from == until) {
            assertTrue(rows.isEmpty(), "empty range produced $rows")
            return
        }
        assertEquals(from, rows.first().from, "first row does not start at $from")
        assertEquals(until, rows.last().until, "last row does not end at $until")
        rows.zipWithNext { a, b -> assertEquals(a.until, b.from, "gap or overlap between $a and $b") }
    }

    @Test
    fun `an empty range produces no rows`() {
        assertEquals(emptyList<JustifiedRow>(), RowSolver.solve(FloatArray(0), 0, 0, target, width, spacing))
    }

    @Test
    fun `a single square asset becomes one trailing row capped at the stretch limit`() {
        val aspects = floatArrayOf(1f)
        val rows = RowSolver.solve(aspects, 0, 1, target, width, spacing)
        assertEquals(1, rows.size)
        // Justifying one square to 1000px wide would be 1000px tall; the cap holds it down.
        assertEquals(target * 1.5f, rows.single().height)
    }

    @Test
    fun `uniform 3x2 assets break into rows that sit near the target height`() {
        val aspects = FloatArray(5) { 1.5f }
        val rows = RowSolver.solve(aspects, 0, 5, target, width, spacing)
        assertRowsTile(rows, 0, 5)
        assertRowsFillWidth(aspects, rows)
        // Three 3:2 assets justify to 220px against a 200px target; squeezing all five
        // into one row would be 131px, which is materially worse, so the solver splits.
        assertEquals(3, rows.first().count)
        assertEquals((width - spacing * 2) / 4.5f, rows.first().height, 0.001f)
    }

    @Test
    fun `a panorama opens its own row rather than crushing the row before it`() {
        // Eight portraits, which justify to roughly the target height on their own,
        // then a very wide panorama. Folding the panorama in would drag all nine down to
        // well under half the target.
        val aspects = FloatArray(9) { if (it == 8) 6f else 0.66f }
        val rows = RowSolver.solve(aspects, 0, 9, target, width, spacing)
        assertRowsTile(rows, 0, 9)
        assertTrue(rows.size >= 2, "expected the panorama to break the row, got $rows")
        assertTrue(
            rows.first().until <= 8,
            "the panorama was folded into the portrait row: ${rows.first()}",
        )
    }

    @Test
    fun `rows tile the range and fill the width across a large random library`() {
        val random = Random(0xC4B5)
        // A plausible mix: mostly 3:2 and 4:3 both ways up, with occasional panoramas.
        val aspects = FloatArray(5_000) {
            when (random.nextInt(10)) {
                0 -> random.nextFloat() * 3f + 2f
                in 1..5 -> 1.5f
                in 6..7 -> 1.333f
                else -> 0.666f
            }
        }
        val rows = RowSolver.solve(aspects, 0, aspects.size, target, width, spacing)
        assertRowsTile(rows, 0, aspects.size)
        assertRowsFillWidth(aspects, rows)
    }

    @Test
    fun `every justified row lands near the target height`() {
        val random = Random(7)
        val aspects = FloatArray(2_000) { random.nextFloat() * 2f + 0.5f }
        val rows = RowSolver.solve(aspects, 0, aspects.size, target, width, spacing)
        // The trailing row is capped rather than justified, so it is excluded.
        rows.dropLast(1).forEach { row ->
            assertTrue(
                row.height in (target * 0.55f)..(target * 1.45f),
                "row $row strayed far from the target height $target",
            )
        }
    }

    @Test
    fun `solving a sub-range matches solving that range alone`() {
        val random = Random(99)
        val aspects = FloatArray(500) { random.nextFloat() * 2f + 0.4f }
        val slice = aspects.copyOfRange(100, 300)
        val fromWhole = RowSolver.solve(aspects, 100, 300, target, width, spacing)
        val fromSlice = RowSolver.solve(slice, 0, slice.size, target, width, spacing)
        assertEquals(fromSlice.size, fromWhole.size)
        fromWhole.zip(fromSlice).forEach { (whole, sliced) ->
            assertEquals(sliced.from + 100, whole.from)
            assertEquals(sliced.until + 100, whole.until)
            assertEquals(sliced.height, whole.height, 0.001f)
        }
    }

    @Test
    fun `the same inputs always produce identical rows`() {
        val random = Random(1234)
        val aspects = FloatArray(1_000) { random.nextFloat() * 2f + 0.4f }
        val first = RowSolver.solve(aspects, 0, aspects.size, target, width, spacing)
        val second = RowSolver.solve(aspects, 0, aspects.size, target, width, spacing)
        assertEquals(first, second)
    }

    @Test
    fun `a narrower viewport yields more rows and a wider one fewer`() {
        val aspects = FloatArray(300) { 1.5f }
        val narrow = RowSolver.solve(aspects, 0, aspects.size, target, 600f, spacing)
        val wide = RowSolver.solve(aspects, 0, aspects.size, target, 1600f, spacing)
        assertTrue(narrow.size > wide.size, "narrow=${narrow.size} wide=${wide.size}")
    }

    @Test
    fun `zero spacing is allowed and rows still fill the width`() {
        val aspects = FloatArray(50) { 1.4f }
        val rows = RowSolver.solve(aspects, 0, aspects.size, target, width, 0f)
        assertRowsTile(rows, 0, aspects.size)
        rows.dropLast(1).forEach { row ->
            val total = (row.from until row.until).sumOf { RowSolver.widthOf(aspects, row, it).toDouble() }
            assertTrue(abs(total - width) < 0.5, "row $row spans $total")
        }
    }
}
