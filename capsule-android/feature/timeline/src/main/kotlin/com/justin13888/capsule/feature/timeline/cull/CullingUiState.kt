package com.justin13888.capsule.feature.timeline.cull

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.feature.timeline.TimelineCell
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList

/**
 * Which flag the review is currently showing.
 *
 * A filter over the review set rather than a query against the catalog: the set is already
 * in hand and bounded, so filtering is arithmetic instead of a round trip, and toggling
 * between "everything" and "just the rejects" cannot fail halfway.
 */
enum class CullFilter {
    /** Every frame in the review set. */
    All,

    /** Only frames the photographer has kept. */
    Picks,

    /** Only frames with no decision on them yet. */
    Undecided,

    /** Only frames the photographer has tossed. */
    Rejects,
}

/**
 * One frame of a review pass.
 *
 * [flag] is held beside [cell] rather than inside it because a keystroke has to be visible
 * before the write lands: at one frame a second a photographer would otherwise be flagging
 * against a picture that has already moved on. The repository write follows, and the
 * catalog's own re-emission agrees with what is already on screen.
 *
 * A `null` [flag] is a token a newer peer wrote that this build does not recognise. It is
 * shown as undecided but never *counted* as undecided, and never swept: refusing to let an
 * unknown value drive a destructive batch is the whole point of preserving it.
 *
 * @param asset the frame's index in the timeline ordering, so the grid can be scrolled back
 *   to wherever the review left off.
 */
@Immutable
data class CullFrame(
    val asset: Int,
    val cell: TimelineCell,
    val flag: CullFlag?,
) {
    /** The asset this frame reviews. */
    val assetId: AssetId get() = cell.assetId

    /** Whether [filter] admits this frame. */
    fun matches(filter: CullFilter): Boolean =
        when (filter) {
            CullFilter.All -> true
            CullFilter.Picks -> flag == CullFlag.Pick
            CullFilter.Undecided -> flag == CullFlag.Neutral
            CullFilter.Rejects -> flag == CullFlag.Reject
        }
}

/**
 * How the review set breaks down, for the filter chips and the sweep button.
 *
 * @param total every frame in the set, whatever its flag.
 * @param picks frames kept.
 * @param undecided frames with no decision yet.
 * @param rejects frames tossed — the batch the sweep would move to the trash.
 */
@Immutable
data class CullCounts(
    val total: Int,
    val picks: Int,
    val undecided: Int,
    val rejects: Int,
)

/**
 * A culling pass in flight.
 *
 * Deliberately a *bounded* review set rather than the whole library. Culling is what a
 * photographer does to one shoot: the set is small enough to hold entirely, which is what
 * lets the flag filter be exact, the position counter be honest, and "move rejects to trash"
 * name a number the user can check rather than a query they have to trust.
 *
 * Every transition below is pure and returns a new value, so the state machine is testable
 * without a renderer and without a repository — the view model applies a transition and
 * writes the consequence, never the other way round.
 *
 * @param frames the review set in timeline order.
 * @param filter which flag the one-up and the filmstrip are restricted to.
 * @param position where the one-up sits inside [visible].
 * @param isConfirmingSweep whether the destructive batch is waiting on a confirmation.
 * @param origin the timeline index the review was entered from, so leaving it returns there.
 */
@Immutable
data class CullingUiState(
    val frames: ImmutableList<CullFrame>,
    val filter: CullFilter = CullFilter.All,
    val position: Int = 0,
    val isConfirmingSweep: Boolean = false,
    val origin: Int = 0,
) {
    /** The frames [filter] admits, in order. */
    val visible: ImmutableList<CullFrame> = frames.filter { it.matches(filter) }.toImmutableList()

    /** The frame under review, or `null` when the filter admits nothing. */
    val current: CullFrame? = visible.getOrNull(position)

    /** How the whole set breaks down, regardless of [filter]. */
    val counts: CullCounts =
        CullCounts(
            total = frames.size,
            picks = frames.count { it.flag == CullFlag.Pick },
            undecided = frames.count { it.flag == CullFlag.Neutral },
            rejects = frames.count { it.flag == CullFlag.Reject },
        )

    /** Whether there is anything at all to review under the current filter. */
    val isEmpty: Boolean get() = visible.isEmpty()

    /**
     * The same pass with [flag] recorded against the frame under review.
     *
     * The position is *not* advanced here. Advancing is a separate decision the caller makes,
     * because flagging from the filmstrip should leave the eye where it is while flagging
     * from the one-up should move on — and a transition that always advanced would make the
     * first of those impossible.
     */
    fun withFlag(flag: CullFlag): CullingUiState {
        val target = current ?: return this
        val updated =
            frames
                .map { if (it.asset == target.asset) it.copy(flag = flag) else it }
                .toImmutableList()
        // Re-clamping matters: under a flag filter, flagging the frame you are looking at
        // removes it from the visible set, and the position must land on its neighbour
        // rather than off the end.
        return copy(frames = updated).clamped()
    }

    /** The same pass moved [delta] frames through the visible set, clamped at both ends. */
    fun movedBy(delta: Int): CullingUiState = movedTo(position + delta)

    /** The same pass with the one-up at [target], clamped into the visible set. */
    fun movedTo(target: Int): CullingUiState {
        if (visible.isEmpty()) return copy(position = 0)
        return copy(position = target.coerceIn(0, visible.lastIndex))
    }

    /**
     * The same pass under a different [filter], keeping the frame under review if it survives.
     *
     * Landing on the same photograph across a filter change is what makes the chips feel like
     * a lens rather than a jump: the alternative — resetting to the first frame — loses the
     * photographer's place every time they check how many rejects they have.
     */
    fun withFilter(filter: CullFilter): CullingUiState {
        val anchor = current?.asset
        val next = copy(filter = filter, position = 0)
        val restored = anchor?.let { asset -> next.visible.indexOfFirst { it.asset == asset } } ?: -1
        return if (restored >= 0) next.copy(position = restored) else next.clamped()
    }

    /** The same pass with the sweep confirmation shown or dismissed. */
    fun confirmingSweep(confirming: Boolean): CullingUiState = copy(isConfirmingSweep = confirming)

    /** The assets the sweep would move to the trash, in review order. */
    fun rejectedAssets(): List<AssetId> = frames.filter { it.flag == CullFlag.Reject }.map(CullFrame::assetId)

    /**
     * The same pass with [removed] taken out — what a completed sweep leaves behind.
     *
     * The swept frames are dropped rather than left greyed out. They are in the trash, which
     * is a different view with its own retention countdown; leaving them here would invite a
     * second sweep of assets already swept.
     */
    fun without(removed: Set<AssetId>): CullingUiState =
        copy(
            frames = frames.filterNot { it.assetId in removed }.toImmutableList(),
            isConfirmingSweep = false,
        ).clamped()

    private fun clamped(): CullingUiState = movedTo(position)

    companion object {
        /** A review over [frames], starting at the frame the user entered from. */
        fun over(
            frames: List<CullFrame>,
            origin: Int,
        ): CullingUiState {
            val ordered = frames.sortedBy(CullFrame::asset)
            val start = ordered.indexOfFirst { it.asset >= origin }.coerceAtLeast(0)
            return CullingUiState(
                frames = ordered.toImmutableList(),
                position = start,
                origin = origin,
            )
        }

        /** An empty review — what a set with nothing in it collapses to. */
        val EMPTY = CullingUiState(frames = persistentListOf())
    }
}
