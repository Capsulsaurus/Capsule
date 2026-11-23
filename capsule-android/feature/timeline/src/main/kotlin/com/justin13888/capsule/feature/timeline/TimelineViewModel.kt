package com.justin13888.capsule.feature.timeline

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.data.SettingsRepository
import com.justin13888.capsule.core.data.StackRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.data.TimelineIndexSource
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.core.ui.gallery.GalleryCellSync
import com.justin13888.capsule.feature.timeline.cull.CullFilter
import com.justin13888.capsule.feature.timeline.cull.CullFrame
import com.justin13888.capsule.feature.timeline.cull.CullingUiState
import kotlinx.collections.immutable.persistentMapOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.collections.immutable.toPersistentMap
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import java.time.Clock

/**
 * The library listing, its selection actions, and the culling pass that runs over it.
 *
 * One view model for both the grid and the review, because culling is a *mode* of the
 * timeline rather than a destination of its own: it is entered from a selection or from the
 * day under the viewport, it writes through the same repositories, and leaving it returns the
 * grid to where the review ended. Splitting them would mean two owners of one scroll position
 * and two readers of one index.
 *
 * Reads are windowed. The index gives the geometry of the whole library for the cost of two
 * flat arrays, and only the windows the grid is showing are read as records — which is what
 * lets a forty-thousand-asset library open in the time it takes to solve one plan.
 *
 * @param clock supplies the instant a retention window is signed from. A parameter, never a
 *   wall-clock read, so a test can assert on the deadline that travels with a delete.
 */
@Suppress("LongParameterList", "TooManyFunctions")
class TimelineViewModel(
    private val timeline: TimelineRepository,
    private val index: TimelineIndexSource,
    private val assets: AssetRepository,
    private val stacks: StackRepository,
    private val sync: SyncRepository,
    private val sessions: SessionRepository,
    private val settings: SettingsRepository,
    private val clock: Clock = Clock.systemUTC(),
) : ViewModel() {
    private val state = MutableStateFlow<TimelineUiState>(TimelineUiState.Loading)

    /** What the screen renders. */
    val uiState: StateFlow<TimelineUiState> = state.asStateFlow()

    /** The window the grid is showing; the loader widens it before it reads. */
    private val visibleRange = MutableStateFlow(0 until INITIAL_WINDOW)

    /** Stack sizes already looked up, so a burst's badge costs one query per stack, not per tile. */
    private val stackSizes = mutableMapOf<StackId, Int>()

    /** The library collector, held so [retry] can replace it rather than stack a second one. */
    private var libraryJob: Job? = null

    init {
        observeLibrary()
        observeWindow()
    }

    /**
     * Records which assets the grid currently has on screen.
     *
     * Driven from the solved plan rather than from what happens to be composed: a fling
     * outruns composition, and reading the windows the *plan* says are visible keeps the tiles
     * ahead of the finger instead of behind it.
     */
    fun onVisibleRange(
        from: Int,
        until: Int,
    ) {
        val clamped = from.coerceAtLeast(0) until until.coerceAtLeast(from + 1)
        if (visibleRange.value != clamped) visibleRange.value = clamped
    }

    /** Re-reads the library after a failure. The remediation behind the error state. */
    fun retry() {
        state.value = TimelineUiState.Loading
        observeLibrary()
    }

    /** Clears the one-shot outcome after the screen has announced it. */
    fun onOutcomeHandled() = updateReady { it.copy(pendingAction = null) }

    /** Clears the pending hand-off once the app module has routed it. */
    fun onHandoffHandled() = updateReady { it.copy(handoff = null) }

    /**
     * Applies [action] to the assets at [indices].
     *
     * Indices rather than ids, because that is what a drag-select produces and a sweep across
     * two thousand tiles must not materialise two thousand records to know what it covers. The
     * ids are resolved here, once, against the windows that hold them.
     */
    fun apply(
        action: TimelineSelectionAction,
        indices: IntArray,
    ) {
        if (indices.isEmpty()) return
        viewModelScope.launch {
            reporting(action, indices.size) {
                val ordered = indices.filter { it >= 0 }.sorted()
                val selected = resolve(ordered)
                when (action) {
                    TimelineSelectionAction.Share -> handOff(TimelineHandoff.Target.Share, selected)
                    TimelineSelectionAction.AddToAlbum -> handOff(TimelineHandoff.Target.AddToAlbum, selected)
                    TimelineSelectionAction.Favourite -> toggleFavourite(selected)
                    TimelineSelectionAction.Hide -> selected.forEach { assets.setHidden(it.id, true) }
                    TimelineSelectionAction.Cull -> openReview(ordered, selected)
                    TimelineSelectionAction.Delete -> softDelete(selected)
                }
            }
        }
    }

    /**
     * Opens the culling review over the day at [section].
     *
     * A day rather than the whole library, deliberately: culling is what a photographer does
     * to one shoot, and a bounded set is what lets the flag filter be exact and "move rejects
     * to trash" name a number the user can check.
     */
    fun reviewSection(section: Int) {
        val ready = state.value as? TimelineUiState.Ready ?: return
        val starts = ready.geometry.sectionStarts
        if (section !in starts.indices) return
        val from = starts[section]
        val until = if (section + 1 < starts.size) starts[section + 1] else ready.geometry.assetCount
        if (until <= from) return
        viewModelScope.launch {
            reporting(TimelineSelectionAction.Cull, until - from) {
                val ordered = (from until until).toList()
                openReview(ordered, resolve(ordered))
            }
        }
    }

    /** Leaves the culling review and returns the grid to where it ended. */
    fun closeReview() = updateReady { it.copy(culling = null) }

    /** Restricts the review to one flag. */
    fun setCullFilter(filter: CullFilter) = updateCulling { it.withFilter(filter) }

    /** Moves the review [delta] frames through the filtered set. */
    fun moveReview(delta: Int) = updateCulling { it.movedBy(delta) }

    /** Jumps the review to [position] in the filtered set — the filmstrip's tap. */
    fun moveReviewTo(position: Int) = updateCulling { it.movedTo(position) }

    /**
     * Flags the frame under review and moves on.
     *
     * Advancing is the point: a review pass is one keystroke per frame, and a tool that made
     * the photographer press "next" between decisions would halve the rate the work gets done
     * at. Under a flag filter the flagged frame leaves the visible set on its own, so the
     * advance is skipped rather than skipping a photograph.
     */
    fun flagCurrent(flag: CullFlag) {
        val target = currentReviewAsset() ?: return
        updateCulling { session ->
            val flagged = session.withFlag(flag)
            if (flagged.current?.asset == session.current?.asset) flagged.movedBy(1) else flagged
        }
        viewModelScope.launch {
            try {
                assets.setCull(target, flag)
            } catch (error: CapsuleError) {
                updateReady {
                    it.copy(pendingAction = TimelineActionOutcome(TimelineSelectionAction.Cull, 1, error.code))
                }
            }
        }
    }

    /** Shows or dismisses the confirmation in front of the destructive sweep. */
    fun confirmSweep(confirming: Boolean) = updateCulling { it.confirmingSweep(confirming) }

    /**
     * Moves every rejected frame in the review to the trash.
     *
     * Soft-delete, with the retention window read from the library's own settings and signed
     * in at delete time — the deadline travels with the deletion rather than being looked up
     * again whenever a purge worker eventually runs.
     */
    fun sweepRejects() {
        val culling = (state.value as? TimelineUiState.Ready)?.culling ?: return
        val rejected = culling.rejectedAssets()
        if (rejected.isEmpty()) {
            updateCulling { it.confirmingSweep(false) }
            return
        }
        viewModelScope.launch {
            reporting(TimelineSelectionAction.Delete, rejected.size) {
                val deadline = clock.instant().plus(settings.settings().first().defaultRetention)
                rejected.forEach { timeline.softDelete(it, deadline) }
                updateCulling { it.without(rejected.toSet()) }
            }
        }
    }

    // ---- Wiring ---------------------------------------------------------------------------

    /**
     * Watches the index, the connection and the session as one.
     *
     * Combined rather than collected separately because the empty state is a *joint* question
     * — an empty library means something different offline, on a device-only library, and on
     * one whose photographs are all in the trash — and three independent collectors would
     * render two of those three answers in sequence on every start.
     */
    private fun observeLibrary() {
        libraryJob?.cancel()
        libraryJob =
            viewModelScope.launch {
                combine(
                    index.timelineIndex().map(TimelineGeometry::from),
                    sync.connectionClass(),
                    sessions.currentSession().map { it != null },
                ) { geometry, connection, signedIn -> LibrarySnapshot(geometry, connection, signedIn) }
                    .collect(::applySnapshot)
            }
    }

    private suspend fun applySnapshot(snapshot: LibrarySnapshot) {
        val reason =
            if (snapshot.geometry.isEmpty) emptyReasonFor(snapshot.connection, snapshot.signedIn) else null
        val previous = state.value as? TimelineUiState.Ready
        // Tiles are keyed by index, so a library whose length changed has re-numbered them.
        // Keeping the old map would draw yesterday's photograph in today's slot for one frame.
        val sameShape = previous?.geometry?.assetCount == snapshot.geometry.assetCount
        state.value =
            TimelineUiState.Ready(
                geometry = snapshot.geometry,
                cells = if (sameShape) previous.cells else persistentMapOf(),
                emptyReason = reason,
                connection = snapshot.connection,
                hasPendingUploads = sameShape && previous.hasPendingUploads,
                culling = previous?.culling,
                pendingAction = previous?.pendingAction,
                handoff = previous?.handoff,
            )
    }

    /**
     * Reads the windows the grid is showing, and re-reads them whenever the catalog changes.
     *
     * Three triggers, and all three are load-bearing. The range, so a scroll fetches what it
     * arrived at. The revision, so a flag written from the review — or a delete swept from it —
     * reaches the tiles without the grid being told which of them moved. And the library's
     * length, because the very first index arrives *after* the first range is reported, and
     * without it the opening screenful would stay blank until the user scrolled.
     *
     * The length rather than the geometry itself: the state is written by this very loader, and
     * a trigger that changed whenever a tile did would be a loop.
     */
    private fun observeWindow() {
        viewModelScope.launch {
            combine(
                visibleRange,
                timeline.revision(),
                state.map { (it as? TimelineUiState.Ready)?.geometry?.assetCount ?: 0 }.distinctUntilChanged(),
            ) { range, _, assetCount -> range to assetCount }
                .collectLatest { (range, assetCount) -> if (assetCount > 0) loadWindow(range) }
        }
    }

    private suspend fun loadWindow(range: IntRange) {
        val ready = state.value as? TimelineUiState.Ready ?: return
        if (ready.geometry.isEmpty) return
        val from = (range.first - OVERREAD).coerceAtLeast(0)
        val until = (range.last + 1 + OVERREAD).coerceAtMost(ready.geometry.assetCount)
        if (until <= from) return
        try {
            val window = timeline.timelineWindow(from, until - from)
            val cells = window.mapIndexed { offset, asset -> (from + offset) to asset.toCell() }
            updateReady { current ->
                current.copy(
                    // Only the window in view is kept. A map that grew with every scroll would
                    // hold the whole library resident, which is what the flat index exists to
                    // avoid in the first place.
                    cells = cells.toMap().toPersistentMap(),
                    hasPendingUploads = cells.any { it.second.markers.sync == GalleryCellSync.Pending },
                )
            }
        } catch (error: CapsuleError) {
            state.value = TimelineUiState.Failed(error.code, error.detail)
        }
    }

    /** Which empty state applies, most specific first. */
    private suspend fun emptyReasonFor(
        connection: ConnectionClass,
        signedIn: Boolean,
    ): TimelineEmptyReason =
        when {
            // Asks only *whether* the trash holds anything, never what is in it: telling an
            // owner "no photos yet" while three hundred sit in their trash is a lie, and the
            // contents stay behind the gated view that owns them.
            hasTrashedAssets() -> TimelineEmptyReason.EverythingFiltered
            connection == ConnectionClass.Offline -> TimelineEmptyReason.Offline
            !signedIn -> TimelineEmptyReason.NeverImportedLocalOnly
            else -> TimelineEmptyReason.NeverImported
        }

    private suspend fun hasTrashedAssets(): Boolean =
        try {
            timeline.trash(0, 1).isNotEmpty()
        } catch (_: CapsuleError) {
            // The wording of an empty state is not worth failing the whole screen over.
            false
        }

    // ---- Actions --------------------------------------------------------------------------

    /**
     * The assets at [ordered], read as contiguous runs.
     *
     * A drag-select is almost always one run, so reading runs rather than one asset at a time
     * turns a two-thousand-tile sweep into a single windowed read.
     */
    private suspend fun resolve(ordered: List<Int>): List<Asset> {
        if (ordered.isEmpty()) return emptyList()
        val out = ArrayList<Asset>(ordered.size)
        var cursor = 0
        while (cursor < ordered.size) {
            var end = cursor
            while (end + 1 < ordered.size && ordered[end + 1] == ordered[end] + 1) end++
            val from = ordered[cursor]
            out += timeline.timelineWindow(from, ordered[end] - from + 1)
            cursor = end + 1
        }
        return out
    }

    /**
     * Raises the selection to the top of the rating scale, or clears it when it is already there.
     *
     * A toggle rather than a set, because the toolbar button is the only affordance and a
     * photographer who favourites a run twice expects the second press to undo the first.
     */
    private suspend fun toggleFavourite(selected: List<Asset>) {
        val alreadyFavourite = selected.isNotEmpty() && selected.all { it.rating >= FAVOURITE_RATING }
        val rating = if (alreadyFavourite) 0 else FAVOURITE_RATING
        selected.forEach { assets.setRating(it.id, rating) }
    }

    private suspend fun softDelete(selected: List<Asset>) {
        val deadline = clock.instant().plus(settings.settings().first().defaultRetention)
        selected.forEach { timeline.softDelete(it.id, deadline) }
    }

    private fun handOff(
        target: TimelineHandoff.Target,
        selected: List<Asset>,
    ) {
        val ids = selected.map { it.id.value }.toImmutableList()
        updateReady { it.copy(handoff = TimelineHandoff(target, ids)) }
    }

    private suspend fun openReview(
        ordered: List<Int>,
        selected: List<Asset>,
    ) {
        val frames =
            ordered.zip(selected).take(MAX_REVIEW_FRAMES).map { (asset, record) ->
                CullFrame(asset = asset, cell = record.toCell(), flag = record.cull.knownOrNull)
            }
        updateReady { it.copy(culling = CullingUiState.over(frames, ordered.firstOrNull() ?: 0)) }
    }

    private fun currentReviewAsset(): AssetId? = (state.value as? TimelineUiState.Ready)?.culling?.current?.assetId

    private suspend fun Asset.toCell(): TimelineCell = toTimelineCell(stackSizeOf(this))

    private suspend fun stackSizeOf(asset: Asset): Int? {
        val stackId = asset.stack?.stackId ?: return null
        stackSizes[stackId]?.let { return it }
        return try {
            stacks.stackMembers(stackId).size.also { stackSizes[stackId] = it }
        } catch (_: CapsuleError) {
            // A badge is worth less than the tile under it: a stack whose size cannot be read
            // draws as an ordinary photograph rather than failing the window.
            null
        }
    }

    private fun updateReady(block: (TimelineUiState.Ready) -> TimelineUiState.Ready) {
        val ready = state.value as? TimelineUiState.Ready ?: return
        state.value = block(ready)
    }

    private fun updateCulling(block: (CullingUiState) -> CullingUiState) {
        updateReady { ready -> ready.culling?.let { ready.copy(culling = block(it)) } ?: ready }
    }

    /**
     * Runs [block], recording whatever it ends in as the screen's one-shot outcome.
     *
     * Every batch ends in a statement the user can read — "fifty hidden", or the localized
     * reason fifty were not — because a batch that silently did nothing is indistinguishable
     * from one that silently did the wrong thing.
     */
    private suspend fun reporting(
        action: TimelineSelectionAction,
        count: Int,
        block: suspend () -> Unit,
    ) {
        try {
            block()
            updateReady { it.copy(pendingAction = TimelineActionOutcome(action, count)) }
        } catch (error: CapsuleError) {
            updateReady { it.copy(pendingAction = TimelineActionOutcome(action, count, error.code)) }
        }
    }

    /** The three library-wide facts the empty state is a joint question about. */
    private data class LibrarySnapshot(
        val geometry: TimelineGeometry,
        val connection: ConnectionClass,
        val signedIn: Boolean,
    )

    private companion object {
        /** How many assets are read before the grid has reported a window. */
        const val INITIAL_WINDOW = 120

        /** How far either side of the visible window is read, so a fling has tiles ready. */
        const val OVERREAD = 60

        /**
         * The most frames one review pass covers.
         *
         * "Select all" over a career's work is a selection, not a review. Capping keeps the set
         * small enough to hold whole, which is what makes the filter exact and the sweep count
         * checkable.
         */
        const val MAX_REVIEW_FRAMES = 500
    }
}
