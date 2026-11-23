package com.justin13888.capsule.feature.timeline

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.TimelineIndex
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.DayBucket
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.feature.timeline.cull.CullingUiState
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.ImmutableMap
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.persistentMapOf
import kotlinx.collections.immutable.toImmutableList

/**
 * The whole timeline's shape, with none of its content.
 *
 * A copy of the repository's index rather than the index itself, because the layout engine
 * wants aspect ratios as floats and the contract stores them as one quantized byte each.
 * Decoding forty thousand bytes once per index change is cheaper than decoding the visible
 * ones sixty times a second, and it keeps the plan solvable off the main thread.
 *
 * Marked [Immutable] over mutable arrays deliberately: nothing writes to these after
 * construction, and the alternative — a boxed list per asset — is the allocation the whole
 * flat-array design exists to avoid.
 *
 * @param aspects width ÷ height per asset, in timeline order.
 * @param sectionStarts the first asset of each day, ascending and starting at zero.
 * @param days the calendar day behind each section, in the same order.
 */
@Immutable
class TimelineGeometry(
    val aspects: FloatArray,
    val sectionStarts: IntArray,
    val days: ImmutableList<DayBucket>,
) {
    /** How many assets the timeline holds. */
    val assetCount: Int get() = aspects.size

    /** Whether there is anything at all to lay out. */
    val isEmpty: Boolean get() = aspects.isEmpty()

    companion object {
        /** The geometry of a library with nothing in it. */
        val EMPTY = TimelineGeometry(FloatArray(0), IntArray(0), persistentListOf())

        /**
         * Reads one snapshot of [index].
         *
         * Days holding no assets are dropped rather than carried through. The layout planner
         * requires strictly ascending section starts, and an empty day would repeat the
         * previous offset — a section header over nothing, and a refused plan.
         */
        fun from(index: TimelineIndex): TimelineGeometry {
            if (index.assetCount == 0) return EMPTY
            val days = index.days.filter { it.count > 0 }
            if (days.isEmpty()) return EMPTY
            return TimelineGeometry(
                aspects = FloatArray(index.assetCount) { index.aspectAt(it) },
                sectionStarts = IntArray(days.size) { days[it].startOffset },
                days = days.toImmutableList(),
            )
        }
    }
}

/**
 * Why the timeline has nothing to show.
 *
 * Separated because the remedy is different in each case, and a library with three hundred
 * photographs in the trash that says "No photos yet" is lying to its owner.
 */
enum class TimelineEmptyReason {
    /** Nothing has ever been imported, and there is a server session behind the library. */
    NeverImported,

    /** Nothing has ever been imported and the user has never signed in — a device-only library. */
    NeverImportedLocalOnly,

    /** The device has no usable network path, so nothing new can arrive from the server. */
    Offline,

    /** The library holds assets, but every one of them is hidden, stacked away, or trashed. */
    EverythingFiltered,
}

/**
 * What the timeline screen is showing.
 *
 * One sealed value rather than a bag of nullable fields, so a screen cannot render a grid and
 * a spinner at once and a `when` catches a state nobody drew.
 */
@Immutable
sealed interface TimelineUiState {
    /** The index has not been read yet. */
    data object Loading : TimelineUiState

    /**
     * The library could not be read.
     *
     * @param code the stable `error.*` key a client localizes, or `null` for a purely local
     *   failure that never crossed a wire and so has no catalogue entry.
     * @param detail the English detail message, kept for a support bundle rather than shown.
     */
    data class Failed(
        val code: ErrorCode?,
        val detail: String,
    ) : TimelineUiState

    /**
     * The index is in hand, whether or not it holds anything.
     *
     * @param geometry where every tile goes.
     * @param cells the tiles read so far, keyed by index into [geometry]. Sparse on purpose:
     *   a scrolled view holds the windows it is showing and lets the rest go.
     * @param emptyReason non-null exactly when [geometry] is empty.
     * @param connection the device's current path quality.
     * @param hasPendingUploads whether any tile read so far still owes the server bytes.
     * @param culling the review session in flight, or `null` while the grid is showing.
     * @param pendingAction a one-shot outcome the screen should announce and then clear.
     * @param handoff a selection waiting to be routed at another feature, or `null`.
     */
    data class Ready(
        val geometry: TimelineGeometry,
        val cells: ImmutableMap<Int, TimelineCell> = persistentMapOf(),
        val emptyReason: TimelineEmptyReason? = null,
        val connection: ConnectionClass = ConnectionClass.Unmetered,
        val hasPendingUploads: Boolean = false,
        val culling: CullingUiState? = null,
        val pendingAction: TimelineActionOutcome? = null,
        val handoff: TimelineHandoff? = null,
    ) : TimelineUiState {
        /** Whether the device can reach anything at all. */
        val isOffline: Boolean get() = connection == ConnectionClass.Offline

        /** The tile at [asset], or `null` when its window has not been read yet. */
        fun cellAt(asset: Int): TimelineCell? = cells[asset]
    }
}

/**
 * The result of a selection action, for the screen to announce once.
 *
 * Carried in the state rather than emitted down a channel: a rotation must not swallow the
 * confirmation that fifty photographs were just hidden, and a state field survives one where
 * an event does not.
 *
 * @param action which action ran.
 * @param count how many assets it touched.
 * @param failure the error it ended in, or `null` when it succeeded.
 */
@Immutable
data class TimelineActionOutcome(
    val action: TimelineSelectionAction,
    val count: Int,
    val failure: ErrorCode? = null,
)

/**
 * A selection this feature has resolved but is not allowed to act on itself.
 *
 * Sharing and filing into an album belong to other product areas, and a feature never depends
 * on another feature — so the timeline resolves the ids and parks them here, and the app
 * module routes them. Carried in the state rather than emitted as an event so a rotation
 * mid-hand-off does not silently drop fifty photographs on the floor.
 *
 * The ids travel as `String` because that is what a navigation key carries; the value class is
 * re-applied wherever the id is used.
 *
 * @param target which surface the selection is bound for.
 * @param assetIds the resolved selection, in timeline order.
 */
@Immutable
data class TimelineHandoff(
    val target: Target,
    val assetIds: ImmutableList<String>,
) {
    /** The surfaces a timeline selection can be handed to. */
    enum class Target {
        /** Mint a share link over the selection. */
        Share,

        /** File the selection into an album. */
        AddToAlbum,
    }
}

/**
 * What the floating toolbar can do to a selection.
 *
 * Closed on purpose. Every entry is a write this feature is allowed to make through
 * `core:data`, so a toolbar button that has no repository behind it cannot be added by
 * accident.
 */
enum class TimelineSelectionAction {
    /** Hand the selection to the sharing surface. Needs a usable network path. */
    Share,

    /** File the selection into an album. */
    AddToAlbum,

    /** Raise the selection to the top of the rating scale. See [FAVOURITE_RATING]. */
    Favourite,

    /** Drop the selection out of every default view without deleting it. */
    Hide,

    /** Open the culling review over the selection. */
    Cull,

    /** Soft-delete the selection under its signed retention window. */
    Delete,
}
