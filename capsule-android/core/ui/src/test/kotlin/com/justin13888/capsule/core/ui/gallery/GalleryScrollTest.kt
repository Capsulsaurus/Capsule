package com.justin13888.capsule.core.ui.gallery

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import kotlin.math.abs
import kotlin.random.Random

/**
 * Each of these pins down a claim the timeline chrome makes to the user: where the
 * scrubber thumb sits, which day the bubble names, and that a photo does not move out
 * from under the fingers during a pinch.
 */
class GalleryScrollTest {
    private val viewportHeight = 2000f
    private val metrics = GalleryMetrics(
        viewportWidth = 1080f,
        targetRowHeight = 220f,
        spacing = 4f,
        headerHeight = 56f,
        sectionSpacing = 16f,
    )

    private fun plan(
        assets: Int = 5_000,
        sections: Int = 80,
        seed: Int = 3,
        metrics: GalleryMetrics = this.metrics,
    ): LayoutPlan {
        val random = Random(seed)
        val aspects = FloatArray(assets) { if (random.nextInt(4) == 0) 0.666f else 1.5f }
        val starts = sortedSetOf(0)
        while (starts.size < sections) starts += random.nextInt(1, assets)
        return LayoutPlanner.plan(aspects, starts.toIntArray(), metrics)
    }

    @Test
    fun `the scrubber fraction spans zero to one across the scrollable range`() {
        val plan = plan()
        assertEquals(0f, GalleryScroll.fractionOf(plan, 0f, viewportHeight))
        assertEquals(1f, GalleryScroll.fractionOf(plan, GalleryScroll.maxScroll(plan, viewportHeight), viewportHeight))
    }

    @Test
    fun `scrubber fraction and scroll offset round trip`() {
        val plan = plan()
        for (step in 0..20) {
            val fraction = step / 20f
            val scroll = GalleryScroll.scrollForFraction(plan, fraction, viewportHeight)
            assertEquals(fraction, GalleryScroll.fractionOf(plan, scroll, viewportHeight), 0.0001f)
        }
    }

    @Test
    fun `a library shorter than the viewport pins the scrubber at the start`() {
        val plan = LayoutPlanner.plan(FloatArray(3) { 1.5f }, intArrayOf(0), metrics)
        assertEquals(0f, GalleryScroll.maxScroll(plan, viewportHeight))
        assertEquals(0f, GalleryScroll.fractionOf(plan, 500f, viewportHeight))
    }

    @Test
    fun `the date bubble names the section actually on screen`() {
        val plan = plan()
        for (section in 0 until plan.sectionCount) {
            val asset = plan.sectionStart(section)
            val slot = plan.slotOfAsset(asset)
            // Scroll so this section's first row sits just below the viewport top.
            val scrollY = plan.offsetOf(slot)
            assertEquals(
                section,
                GalleryScroll.sectionAt(plan, scrollY),
                "scrolling to section $section named a different one",
            )
        }
    }

    @Test
    fun `zoom anchoring keeps the focused asset in place`() {
        val dense = plan(metrics = metrics.copy(targetRowHeight = 140f))
        val sparse = plan(metrics = metrics.copy(targetRowHeight = 320f))
        val scrollY = GalleryScroll.maxScroll(dense, viewportHeight) * 0.42f

        val anchor = GalleryScroll.captureAnchor(dense, scrollY, viewportHeight)
        assertNotNull(anchor)
        requireNotNull(anchor)

        val restored = GalleryScroll.applyAnchor(sparse, anchor, viewportHeight)
        val before = plan(metrics = metrics.copy(targetRowHeight = 140f))
            .let { it.offsetOf(it.slotOfAsset(anchor.asset)) - scrollY }
        val after = sparse.offsetOf(sparse.slotOfAsset(anchor.asset)) - restored

        // The anchored asset sits at the same place in the viewport, within a row height.
        assertTrue(
            abs(before - after) < 320f,
            "anchor asset ${anchor.asset} moved from $before to $after",
        )
    }

    @Test
    fun `anchoring near the end settles at the end rather than overscrolling`() {
        val plan = plan()
        val anchor = GalleryScroll.Anchor(plan.assetCount - 1, 0f)
        val scroll = GalleryScroll.applyAnchor(plan, anchor, viewportHeight)
        assertTrue(scroll <= GalleryScroll.maxScroll(plan, viewportHeight) + 0.001f)
        assertTrue(scroll >= 0f)
    }

    @Test
    fun `visible slots cover the viewport and nothing far outside it`() {
        val plan = plan()
        val scrollY = 12_000f
        val range = GalleryScroll.visibleSlots(plan, scrollY, viewportHeight)
        // Everything intersecting the viewport is inside the range.
        for (slot in 0 until plan.slotCount) {
            val top = plan.offsetOf(slot)
            val bottom = top + plan.heightOf(slot)
            val intersects = bottom > scrollY && top < scrollY + viewportHeight
            if (intersects) {
                assertTrue(slot in range, "slot $slot intersects the viewport but is outside $range")
            }
        }
        // And the range does not run away past the viewport.
        assertTrue(plan.offsetOf(range.first) <= scrollY)
        assertTrue(plan.offsetOf(range.last) < scrollY + viewportHeight)
    }

    @Test
    fun `overscan widens the composed range downward`() {
        val plan = plan()
        val tight = GalleryScroll.visibleSlots(plan, 12_000f, viewportHeight)
        val loose = GalleryScroll.visibleSlots(plan, 12_000f, viewportHeight, overscan = 1_500f)
        assertTrue(loose.last >= tight.last)
        assertTrue(loose.first <= tight.first)
    }

    @Test
    fun `an empty plan yields no visible slots`() {
        val plan = LayoutPlanner.plan(FloatArray(0), intArrayOf(0), metrics)
        assertEquals(1, plan.slotCount) // the section header alone
        assertEquals(0, GalleryScroll.visibleSlots(plan, 0f, viewportHeight).first)
    }
}
