package com.justin13888.capsule.core.ui.gallery

/**
 * The scroll arithmetic the timeline's chrome is built on.
 *
 * All of it is pure and lives beside the layout rather than inside a composable, because
 * every one of these is a claim the UI makes to the user and therefore something tests
 * should be able to pin down: the scrubber thumb's position claims "you are this far
 * through your library", the date bubble claims "you are looking at this day", and zoom
 * anchoring claims "the photo under your fingers did not move".
 */
object GalleryScroll {
    /**
     * An asset to hold still across a re-plan, and where it sat in the viewport.
     *
     * Captured before a zoom or rotation and applied to the new plan afterwards. Anchoring
     * to an *asset* rather than to a pixel offset is what makes the gesture feel like the
     * grid is resizing around the user's fingers rather than jumping.
     *
     * @param asset index of the asset to keep in place.
     * @param viewportFraction where it sat vertically, 0 at the viewport top, 1 at the bottom.
     */
    data class Anchor(val asset: Int, val viewportFraction: Float)

    /** The furthest a plan can be scrolled while keeping content on screen. */
    fun maxScroll(plan: LayoutPlan, viewportHeight: Float): Float =
        (plan.totalHeight - viewportHeight).coerceAtLeast(0f)

    /**
     * How far through the library [scrollY] is, from 0 to 1.
     *
     * This drives the scrubber thumb. It is a fraction of *scrollable* range, not of
     * total height, so the thumb reaches the bottom exactly when the content does.
     */
    fun fractionOf(plan: LayoutPlan, scrollY: Float, viewportHeight: Float): Float {
        val max = maxScroll(plan, viewportHeight)
        return if (max <= 0f) 0f else (scrollY / max).coerceIn(0f, 1f)
    }

    /** The scroll offset a scrubber [fraction] maps to. Inverse of [fractionOf]. */
    fun scrollForFraction(plan: LayoutPlan, fraction: Float, viewportHeight: Float): Float =
        maxScroll(plan, viewportHeight) * fraction.coerceIn(0f, 1f)

    /**
     * The section whose content occupies the top of the viewport at [scrollY].
     *
     * This is what the date bubble and the sticky header show. It resolves through the
     * slot at that offset rather than by interpolating over the scroll range, so the
     * label always names the day actually on screen.
     */
    fun sectionAt(plan: LayoutPlan, scrollY: Float): Int {
        val slot = plan.slotAt(scrollY.coerceAtLeast(0f))
        if (slot < 0) return -1
        if (plan.isHeader(slot)) return plan.sectionOf(slot)
        return plan.sectionOfAsset(plan.rowFrom(slot))
    }

    /**
     * Capture the asset to hold still across a re-plan.
     *
     * Prefers whatever sits at the vertical centre of the viewport: it is the most likely
     * subject of attention, and unlike the topmost asset it does not change identity every
     * time a row boundary crosses the viewport edge.
     */
    fun captureAnchor(plan: LayoutPlan, scrollY: Float, viewportHeight: Float): Anchor? {
        val focusY = scrollY + viewportHeight / 2f
        val slot = plan.slotAt(focusY)
        if (slot < 0) return null
        val asset = if (plan.isHeader(slot)) {
            val next = slot + 1
            if (next >= plan.slotCount || plan.isHeader(next)) return null
            plan.rowFrom(next)
        } else {
            plan.rowFrom(slot)
        }
        val slotOfAsset = plan.slotOfAsset(asset)
        if (slotOfAsset < 0) return null
        val fraction = if (viewportHeight <= 0f) {
            0f
        } else {
            ((plan.offsetOf(slotOfAsset) - scrollY) / viewportHeight).coerceIn(0f, 1f)
        }
        return Anchor(asset, fraction)
    }

    /**
     * The scroll offset in [plan] that puts [anchor] back where it was.
     *
     * Clamped to the plan's scrollable range, so anchoring near either end settles at the
     * end rather than overscrolling.
     */
    fun applyAnchor(plan: LayoutPlan, anchor: Anchor, viewportHeight: Float): Float {
        val slot = plan.slotOfAsset(anchor.asset)
        if (slot < 0) return 0f
        val target = plan.offsetOf(slot) - anchor.viewportFraction * viewportHeight
        return target.coerceIn(0f, maxScroll(plan, viewportHeight))
    }

    /**
     * The slots that intersect the viewport at [scrollY], widened by [overscan] pixels
     * above and below so the next row is composed before it scrolls into view.
     *
     * Returned as a range because slots are contiguous and ordered; the renderer walks it
     * directly rather than filtering the whole plan.
     */
    fun visibleSlots(
        plan: LayoutPlan,
        scrollY: Float,
        viewportHeight: Float,
        overscan: Float = 0f,
    ): IntRange {
        if (plan.slotCount == 0) return IntRange.EMPTY
        val top = (scrollY - overscan).coerceAtLeast(0f)
        val bottom = scrollY + viewportHeight + overscan
        val first = plan.slotAt(top).coerceAtLeast(0)
        var last = first
        while (last + 1 < plan.slotCount && plan.offsetOf(last + 1) < bottom) last++
        return first..last
    }
}
