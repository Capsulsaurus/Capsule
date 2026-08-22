package com.justin13888.capsule.core.ui.gallery

/**
 * One laid-out row of a justified gallery: a contiguous slice of the asset list, scaled
 * to a common height so the row fills the viewport width exactly.
 *
 * Rows are described by index range rather than by content so the layout can be solved
 * from the timeline index alone — a byte of aspect ratio per asset — without loading a
 * single thumbnail. That is what lets a million-asset library lay out exactly rather
 * than being estimated and corrected as the user scrolls.
 *
 * @param from index of the first asset in the row, inclusive.
 * @param until index one past the last asset in the row.
 * @param height the common height every asset in the row is drawn at, in pixels.
 */
data class JustifiedRow(
    val from: Int,
    val until: Int,
    val height: Float,
) {
    init {
        require(until > from) { "a row needs at least one asset, got [$from, $until)" }
        require(height > 0f) { "row height must be positive, got $height" }
    }

    /** How many assets the row holds. */
    val count: Int get() = until - from
}
