package com.justin13888.capsule.core.ui.gallery

import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The measurements the timeline is drawn at.
 *
 * These live beside the layout engine rather than in the design system because they are
 * not a visual language — they are the arguments a [GalleryMetrics] is solved from, and a
 * feature that changes one of them changes the plan, not just the paint.
 */
object GalleryDefaults {
    /** The gap between adjacent thumbnails, both axes. */
    val Spacing: Dp = 2.dp

    /** The height of a day header. */
    val HeaderHeight: Dp = 44.dp

    /** Extra breathing room above every header but the first. */
    val SectionSpacing: Dp = 12.dp

    /**
     * How far beyond the viewport rows are composed.
     *
     * Roughly one target row in each direction. Larger buys smoother flings at the cost
     * of holding more decoded bitmaps; smaller shows blank rows at fling speed.
     */
    val Overscan: Dp = 240.dp

    /** The width of the date scrubber's touch rail. */
    val ScrubberWidth: Dp = 48.dp

    /** The height of the scrubber thumb — also its minimum touch target. */
    val ScrubberThumbHeight: Dp = 48.dp

    /** How long the scrubber stays visible after the last scroll. */
    const val SCRUBBER_IDLE_MILLIS: Long = 1_600L

    /** The corner radius a thumbnail is clipped to when nothing is selected. */
    val CellCorner: Dp = 4.dp

    /**
     * Builds the pixel metrics a plan is solved against from this module's Dp defaults.
     *
     * Offered as a helper because the conversion has to happen exactly once, against the
     * density the gallery will actually be measured at — a feature that rounds to pixels
     * itself will produce a plan whose rows do not fill the viewport.
     *
     * @param viewportWidth content width in pixels, insets already removed.
     * @param targetRowHeight the height rows aim for, in pixels; see
     *   [GalleryDensityLevel.targetRowHeight].
     */
    @Composable
    fun metrics(
        viewportWidth: Float,
        targetRowHeight: Float,
        spacing: Dp = Spacing,
        headerHeight: Dp = HeaderHeight,
        sectionSpacing: Dp = SectionSpacing,
    ): GalleryMetrics {
        val density = LocalDensity.current
        return GalleryMetrics(
            viewportWidth = viewportWidth,
            targetRowHeight = targetRowHeight,
            spacing = with(density) { spacing.toPx() },
            headerHeight = with(density) { headerHeight.toPx() },
            sectionSpacing = with(density) { sectionSpacing.toPx() },
        )
    }
}

/**
 * Test tags for the parts of the gallery a caller cannot address by text.
 *
 * The pinned header and the scrubber have no stable label of their own — their content
 * comes from the caller — so the UI tests would otherwise have to assert against fixture
 * copy. Published rather than internal so a feature's own tests can reach them too.
 */
object GalleryTestTags {
    /** The header pinned to the top of the viewport. */
    const val STICKY_HEADER: String = "capsule.gallery.stickyHeader"

    /** The scrubber's draggable rail. */
    const val SCRUBBER: String = "capsule.gallery.scrubber"

    /** The scrubber's date bubble. */
    const val SCRUBBER_BUBBLE: String = "capsule.gallery.scrubber.bubble"

    /** The scrolling grid itself. */
    const val GRID: String = "capsule.gallery.grid"
}
