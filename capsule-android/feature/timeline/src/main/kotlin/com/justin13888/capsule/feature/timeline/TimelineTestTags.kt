package com.justin13888.capsule.feature.timeline

/**
 * Handles for the parts of the timeline a test cannot address by text.
 *
 * Published rather than internal because the app module's shell tests assert that filling a
 * destination in did not change what the shell does around it, and a tag they cannot see is
 * a tag they would have to duplicate as a string literal.
 *
 * Anything with a visible, catalogued label is deliberately absent: asserting against the
 * label is asserting against what the user actually reads, and a tag there would let the copy
 * rot without a test noticing.
 */
object TimelineTestTags {
    /** The whole timeline screen, whichever state it is in. */
    const val ROOT: String = "timeline.root"

    /** The wavy indicator shown while the index is being read. */
    const val LOADING: String = "timeline.loading"

    /** The error state, with its remediation. */
    const val ERROR: String = "timeline.error"

    /** The empty state. Its body names which of the empty reasons applies. */
    const val EMPTY: String = "timeline.empty"

    /** The banner explaining that the last batch was refused, and why. */
    const val ACTION_FAILURE: String = "timeline.actionFailure"

    /** The strip explaining that uploads are stalled on a missing connection. */
    const val OFFLINE_STRIP: String = "timeline.offline"

    /** The floating toolbar shown in selection mode. */
    const val SELECTION_TOOLBAR: String = "timeline.selection.toolbar"

    /** The culling review, one-up and filmstrip together. */
    const val REVIEW: String = "timeline.review"

    /** The full-bleed frame under review. */
    const val REVIEW_FRAME: String = "timeline.review.frame"

    /** The strip of thumbnails beneath the one-up view. */
    const val REVIEW_FILMSTRIP: String = "timeline.review.filmstrip"

    /** The confirmation in front of the reject sweep. */
    const val REVIEW_SWEEP_CONFIRM: String = "timeline.review.sweep"

    /** One tile of the grid: `timeline.tile:<index>`. */
    fun tile(asset: Int): String = "timeline.tile:$asset"

    /** One thumbnail of the review filmstrip: `timeline.review.frame:<position>`. */
    fun reviewFrame(position: Int): String = "timeline.review.frame:$position"
}
