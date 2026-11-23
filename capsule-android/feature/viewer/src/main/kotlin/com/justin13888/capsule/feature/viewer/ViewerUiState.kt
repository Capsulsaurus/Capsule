package com.justin13888.capsule.feature.viewer

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.RepresentationTier
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.toImmutableList

/**
 * Where the fetch for the rung the viewer wants ended up.
 *
 * The interesting states are the two that are *not* failures. A permanent unavailability
 * degrades down the ladder and keeps drawing; a pending original is transient and re-fetches
 * itself. Collapsing either into an error would make the user believe a photo was lost when
 * nothing was.
 */
@Immutable
sealed interface RepresentationStatus {
    /** A request is in flight, or none has been made for this page yet. */
    data object Resolving : RepresentationStatus

    /** The wanted rung is in hand — nothing to say about it. */
    data class Held(
        val tier: RepresentationTier,
    ) : RepresentationStatus

    /**
     * The wanted rung is permanently unreachable, so the ladder walked down to [held].
     *
     * Kept as its own state rather than as `Held(held)` because the difference is the entire
     * message: the user asked for full resolution and is looking at something smaller, and
     * saying so is what stops them concluding the app quietly resized their photograph.
     */
    data class Degraded(
        /** What the viewer asked for. */
        val wanted: RepresentationTier,
        /** The best rung actually in hand. */
        val held: RepresentationTier,
    ) : RepresentationStatus

    /**
     * The original has not been uploaded from its device yet. Transient, not a loss.
     *
     * Carries the refusal it came from rather than discarding it. `error.blob.pending_upload`
     * is explicitly distinct from a `410`, and keeping the code on the state is what lets the
     * distinction be checked rather than merely commented.
     */
    data class AwaitingOriginal(
        val failure: ViewerFailure,
    ) : RepresentationStatus

    /**
     * The fetch was refused for a reason the user can act on — a revoked album, no network.
     *
     * Distinct from [Degraded]: an authorization change is not a durability loss, so it is
     * surfaced as a revocation and the membership state is re-synced before anything degrades.
     */
    data class Refused(
        val failure: ViewerFailure,
    ) : RepresentationStatus
}

/** One asset in the pager, with everything its own record establishes about it. */
@Immutable
data class ViewerPage(
    /** The asset itself. */
    val asset: Asset,
    /** The conditions the record alone proves, in drawing order. */
    val degradations: ImmutableList<ViewerDegradation>,
) {
    /** Whether this asset may be written to at all. */
    val isEditable: Boolean get() = degradations.none(ViewerDegradation::blocksEditing)

    companion object {
        /** Wraps an asset, deriving its intrinsic conditions. */
        fun of(asset: Asset): ViewerPage = ViewerPage(asset, intrinsicDegradations(asset).toImmutableList())
    }
}

/**
 * Everything the paged viewer draws, as one value.
 *
 * The listing is held whole rather than as a cursor because paging inside a viewer is
 * bidirectional and immediate — a swipe must not wait on a query — and the window is bounded
 * to what a person can plausibly swipe through in one sitting rather than to the library.
 */
@Immutable
sealed interface ViewerUiState {
    /** Resolving the listing and the asset the viewer was opened at. */
    data object Loading : ViewerUiState

    /**
     * The asset is not in this library.
     *
     * An empty state rather than an error: a photo deleted on another device is a thing that
     * legitimately happened, not a fault to retry.
     */
    data object Missing : ViewerUiState

    /** The listing itself could not be read. */
    data class Failed(
        val failure: ViewerFailure,
    ) : ViewerUiState

    /** The pager, its position, and the fetch state of the page being looked at. */
    data class Ready(
        /** Every page the user can swipe to, in listing order. */
        val pages: ImmutableList<ViewerPage>,
        /** Which page is on screen. Always a valid index into [pages]. */
        val index: Int,
        /** Where the current page's representation fetch ended up. */
        val representation: RepresentationStatus,
        /**
         * The label of the device holding an original that has not been uploaded.
         *
         * Resolved from the signed device directory when it is reachable and `null` when it
         * is not — a device *name* is server-held state, and an offline viewer should still
         * be able to say the original is elsewhere even when it cannot say where.
         */
        val originDeviceLabel: String? = null,
    ) : ViewerUiState {
        init {
            // A pager with no pages has no `current`, and every consumer would need a branch
            // for it. The state that means "nothing to show" is [Missing]; keeping the two
            // apart is what stops a caller reaching for `current` on an empty listing.
            require(pages.isNotEmpty()) { "a ready viewer always has at least one page" }
            require(index in pages.indices) { "index $index is outside 0..${pages.size - 1}" }
        }

        /** The page on screen. */
        val current: ViewerPage get() = pages[index]

        /** Every condition worth a banner, intrinsic and fetch-derived together. */
        val degradations: ImmutableList<ViewerDegradation>
            get() =
                when (representation) {
                    is RepresentationStatus.Degraded ->
                        (current.degradations + ViewerDegradation.FullResolutionUnavailable).toImmutableList()

                    is RepresentationStatus.AwaitingOriginal ->
                        if (ViewerDegradation.OriginalOnAnotherDevice in current.degradations) {
                            current.degradations
                        } else {
                            (current.degradations + ViewerDegradation.OriginalOnAnotherDevice).toImmutableList()
                        }

                    else -> current.degradations
                }

        /** Whether paging forward is possible. */
        val hasNext: Boolean get() = index < pages.size - 1

        /** Whether paging back is possible. */
        val hasPrevious: Boolean get() = index > 0
    }
}
