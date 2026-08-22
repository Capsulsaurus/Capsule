package com.justin13888.capsule.core.ui.gallery

import androidx.compose.runtime.Immutable

/**
 * What a header slot is being asked to draw.
 *
 * The gallery renders no copy of its own: it knows a section is assets `[start, end)` of
 * the plan and nothing about what a section *means*. So the header slot is handed the
 * section index and the two facts only the gallery knows — whether this is the pinned copy,
 * and whether the grid is moving — and the caller supplies the date.
 *
 * @param section the section this header belongs to. A date and a count are reachable from
 *   it through [LayoutPlan.sectionStart] and [LayoutPlan.sectionSize].
 * @param isPinned this is the copy stuck to the top of the viewport rather than the one in
 *   the scrolling content. A pinned header needs an opaque background; an in-content one
 *   does not.
 * @param isCollapsed the user is travelling rather than reading, so the header should give
 *   its space back.
 */
@Immutable
data class GalleryHeaderInfo(
    val section: Int,
    val isPinned: Boolean,
    val isCollapsed: Boolean,
)

/**
 * A month marker on the scrubber rail.
 *
 * @param section the section the tick names. Its position on the rail is derived from where
 *   that section actually sits in the plan, not from an even division of the year — a month
 *   holding four thousand photographs occupies more rail than one holding forty, which is
 *   the only version of this control that does not lie.
 * @param label already-formatted, usually a month or a year.
 */
@Immutable
data class GalleryScrubberTick(
    val section: Int,
    val label: String,
)
