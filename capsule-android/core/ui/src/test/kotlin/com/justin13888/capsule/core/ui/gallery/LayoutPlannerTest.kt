package com.justin13888.capsule.core.ui.gallery

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import kotlin.random.Random
import kotlin.system.measureTimeMillis

/**
 * The gallery treats a plan as ground truth for the scrollbar, the date scrubber, zoom
 * anchoring and drag-select hit testing. Every one of those is a lookup into the arrays
 * built here, so the plan's internal consistency is what those features rest on.
 */
class LayoutPlannerTest {
    private val metrics =
        GalleryMetrics(
            viewportWidth = 1080f,
            targetRowHeight = 220f,
            spacing = 4f,
            headerHeight = 56f,
            sectionSpacing = 16f,
        )

    private fun library(
        assets: Int,
        sections: Int,
        seed: Int = 42,
    ): Pair<FloatArray, IntArray> {
        val random = Random(seed)
        val aspects =
            FloatArray(assets) {
                when (random.nextInt(10)) {
                    0 -> random.nextFloat() * 3f + 2f
                    in 1..5 -> 1.5f
                    in 6..7 -> 1.333f
                    else -> 0.666f
                }
            }
        // Uneven sections, like real capture days.
        val starts = sortedSetOf(0)
        while (starts.size < sections) starts += random.nextInt(1, assets)
        return aspects to starts.toIntArray()
    }

    @Test
    fun `a single section produces one header followed by its rows`() {
        val aspects = FloatArray(20) { 1.5f }
        val plan = LayoutPlanner.plan(aspects, intArrayOf(0), metrics)
        assertEquals(1, plan.sectionCount)
        assertTrue(plan.isHeader(0))
        assertEquals(0, plan.sectionOf(0))
        assertEquals(0f, plan.offsetOf(0))
        assertEquals(metrics.headerHeight, plan.heightOf(0))
        (1 until plan.slotCount).forEach { assertTrue(!plan.isHeader(it), "slot $it should be a row") }
    }

    @Test
    fun `slots ascend and tile the content height without gaps or overlap`() {
        val (aspects, starts) = library(assets = 4_000, sections = 60)
        val plan = LayoutPlanner.plan(aspects, starts, metrics)
        var previousBottom = 0f
        for (slot in 0 until plan.slotCount) {
            val top = plan.offsetOf(slot)
            assertTrue(top >= previousBottom - 0.001f, "slot $slot at $top overlaps previous bottom $previousBottom")
            previousBottom = top + plan.heightOf(slot)
        }
        assertEquals(plan.totalHeight, previousBottom, 0.001f)
    }

    @Test
    fun `every asset appears in exactly one row`() {
        val (aspects, starts) = library(assets = 3_000, sections = 40)
        val plan = LayoutPlanner.plan(aspects, starts, metrics)
        val seen = BooleanArray(aspects.size)
        for (slot in 0 until plan.slotCount) {
            if (plan.isHeader(slot)) continue
            for (asset in plan.rowFrom(slot) until plan.rowUntil(slot)) {
                assertTrue(!seen[asset], "asset $asset appeared twice")
                seen[asset] = true
            }
        }
        assertTrue(seen.all { it }, "some assets never appeared in a row")
    }

    @Test
    fun `no row spans a section boundary`() {
        val (aspects, starts) = library(assets = 2_000, sections = 50)
        val plan = LayoutPlanner.plan(aspects, starts, metrics)
        for (slot in 0 until plan.slotCount) {
            if (plan.isHeader(slot)) continue
            val section = plan.sectionOfAsset(plan.rowFrom(slot))
            assertEquals(
                section,
                plan.sectionOfAsset(plan.rowUntil(slot) - 1),
                "row at slot $slot crosses a section boundary",
            )
        }
    }

    @Test
    fun `slotAt finds the slot containing a position`() {
        val (aspects, starts) = library(assets = 1_500, sections = 25)
        val plan = LayoutPlanner.plan(aspects, starts, metrics)
        for (slot in 0 until plan.slotCount) {
            val middle = plan.offsetOf(slot) + plan.heightOf(slot) / 2f
            assertEquals(slot, plan.slotAt(middle), "midpoint of slot $slot resolved elsewhere")
        }
    }

    @Test
    fun `slotOfAsset and sectionOfAsset agree with the slot contents`() {
        val (aspects, starts) = library(assets = 1_200, sections = 20)
        val plan = LayoutPlanner.plan(aspects, starts, metrics)
        for (asset in aspects.indices) {
            val slot = plan.slotOfAsset(asset)
            assertTrue(!plan.isHeader(slot), "asset $asset resolved to header slot $slot")
            assertTrue(
                asset in plan.rowFrom(slot) until plan.rowUntil(slot),
                "asset $asset is not inside slot $slot",
            )
            val section = plan.sectionOfAsset(asset)
            assertTrue(
                asset in plan.sectionStart(section) until plan.sectionEnd(section),
                "asset $asset is not inside section $section",
            )
        }
    }

    @Test
    fun `assetAt hits the asset drawn under a point`() {
        val (aspects, starts) = library(assets = 600, sections = 12)
        val plan = LayoutPlanner.plan(aspects, starts, metrics)
        for (asset in aspects.indices) {
            val slot = plan.slotOfAsset(asset)
            val x = plan.assetX(slot, asset) + plan.assetWidth(slot, asset) / 2f
            val y = plan.offsetOf(slot) + plan.heightOf(slot) / 2f
            assertEquals(asset, plan.assetAt(x, y), "point over asset $asset hit something else")
        }
    }

    @Test
    fun `a point over a header hits no asset`() {
        val (aspects, starts) = library(assets = 400, sections = 8)
        val plan = LayoutPlanner.plan(aspects, starts, metrics)
        val headerSlot = (0 until plan.slotCount).first { plan.isHeader(it) }
        val y = plan.offsetOf(headerSlot) + plan.heightOf(headerSlot) / 2f
        assertEquals(-1, plan.assetAt(10f, y))
    }

    @Test
    fun `re-planning at a new width keeps an anchor asset reachable`() {
        val (aspects, starts) = library(assets = 2_000, sections = 30)
        val wide = LayoutPlanner.plan(aspects, starts, metrics)
        val narrow = LayoutPlanner.plan(aspects, starts, metrics.copy(viewportWidth = 720f))
        // The same asset exists in both plans at different offsets — this is what zoom
        // and rotation anchoring rely on.
        val anchor = 1_234
        assertNotEquals(wide.offsetOfAsset(anchor, 2000f), narrow.offsetOfAsset(anchor, 2000f))
        assertTrue(narrow.totalHeight > wide.totalHeight, "narrower viewport should be taller overall")
    }

    @Test
    fun `a smaller target row height packs more assets per row`() {
        val (aspects, starts) = library(assets = 1_000, sections = 10)
        val dense = LayoutPlanner.plan(aspects, starts, metrics.copy(targetRowHeight = 110f))
        val sparse = LayoutPlanner.plan(aspects, starts, metrics.copy(targetRowHeight = 330f))
        assertTrue(dense.slotCount < sparse.slotCount, "dense=${dense.slotCount} sparse=${sparse.slotCount}")
        assertTrue(dense.totalHeight < sparse.totalHeight)
    }

    @Test
    fun `the same inputs always produce the same plan`() {
        val (aspects, starts) = library(assets = 800, sections = 15)
        val first = LayoutPlanner.plan(aspects, starts, metrics)
        val second = LayoutPlanner.plan(aspects, starts, metrics)
        assertEquals(first.slotCount, second.slotCount)
        assertEquals(first.totalHeight, second.totalHeight)
        for (slot in 0 until first.slotCount) {
            assertEquals(first.offsetOf(slot), second.offsetOf(slot))
            assertEquals(first.heightOf(slot), second.heightOf(slot))
        }
    }

    @Test
    fun `malformed section boundaries are rejected rather than silently mislaid out`() {
        val aspects = FloatArray(10) { 1f }
        assertThrows<IllegalArgumentException> {
            LayoutPlanner.plan(aspects, intArrayOf(), metrics)
        }
        assertThrows<IllegalArgumentException> {
            LayoutPlanner.plan(aspects, intArrayOf(1, 5), metrics)
        }
        assertThrows<IllegalArgumentException> {
            LayoutPlanner.plan(aspects, intArrayOf(0, 5, 5), metrics)
        }
        assertThrows<IllegalArgumentException> {
            LayoutPlanner.plan(aspects, intArrayOf(0, 50), metrics)
        }
    }

    @Test
    fun `a million-asset library plans fast enough to run on every metric change`() {
        val (aspects, starts) = library(assets = 1_000_000, sections = 3_650)
        lateinit var plan: LayoutPlan
        val elapsed = measureTimeMillis { plan = LayoutPlanner.plan(aspects, starts, metrics) }
        assertEquals(1_000_000, plan.assetCount)
        // Generous for CI, but far below the point where this would need estimating
        // instead of solving. Locally it lands around a tenth of this.
        assertTrue(elapsed < 5_000, "planning a million assets took ${elapsed}ms")
    }
}
