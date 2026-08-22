package com.justin13888.capsule.core.ui.gallery

/**
 * The pixel measurements a gallery layout is solved against.
 *
 * Grouped into one value so a plan can be cached by exactly the inputs that produced it:
 * change any of these and the plan must be resolved, change none and the cached plan is
 * still correct.
 *
 * @param viewportWidth the width rows justify to, in pixels — content width, insets already removed.
 * @param targetRowHeight the height rows aim for. This is the zoom control: a smaller
 *   target packs more assets per row.
 * @param spacing the gap between adjacent assets, both horizontally and vertically.
 * @param headerHeight the height of a section header.
 * @param sectionSpacing extra space above each header beyond the first.
 */
data class GalleryMetrics(
    val viewportWidth: Float,
    val targetRowHeight: Float,
    val spacing: Float,
    val headerHeight: Float,
    val sectionSpacing: Float,
) {
    init {
        require(viewportWidth > 0f) { "viewportWidth must be positive, got $viewportWidth" }
        require(targetRowHeight > 0f) { "targetRowHeight must be positive, got $targetRowHeight" }
        require(spacing >= 0f) { "spacing cannot be negative, got $spacing" }
        require(headerHeight >= 0f) { "headerHeight cannot be negative, got $headerHeight" }
        require(sectionSpacing >= 0f) { "sectionSpacing cannot be negative, got $sectionSpacing" }
    }
}
