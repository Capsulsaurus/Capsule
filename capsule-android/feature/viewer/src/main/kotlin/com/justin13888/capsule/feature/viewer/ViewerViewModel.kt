package com.justin13888.capsule.feature.viewer

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.BlobError
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.RepresentationTier
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/**
 * The paged viewer's state machine.
 *
 * Three jobs, kept apart because they fail independently:
 *
 * 1. **The listing.** Read once into a bounded window. Paging is bidirectional and has to be
 *    instant, so a swipe never waits on a query.
 * 2. **The open asset.** Observed, so a rating written in the editor beside the photo is on
 *    screen before the write returns.
 * 3. **The representation.** Fetched on demand for the page being looked at, and *only* that
 *    page — the download path's whole design is fetching the smallest representation that
 *    satisfies the current intent and nothing more.
 *
 * The third is where the degraded states come from, and why its outcome is modelled rather
 * than thrown away: a permanent `410` degrades down the ladder and keeps drawing, a pending
 * original is transient, and a `403` is neither. Reducing all three to "failed" would tell
 * the user their photograph was gone in two cases where it is not.
 */
class ViewerViewModel(
    private val openAt: AssetId,
    private val contextAlbum: AlbumId?,
    private val assets: AssetRepository,
    private val timeline: TimelineRepository,
    private val albums: AlbumRepository,
    private val sync: SyncRepository,
    sessions: SessionRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow<ViewerUiState>(ViewerUiState.Loading)

    /** The one immutable value the screen renders. */
    val state: StateFlow<ViewerUiState> = mutableState.asStateFlow()

    /** The signed device directory, so an awaiting-original badge can name a device. */
    private val deviceNames = MutableStateFlow<Map<String, String>>(emptyMap())

    private var pageJob: Job? = null
    private var fetchJob: Job? = null

    init {
        viewModelScope.launch { observeDeviceDirectory(sessions) }
        load()
    }

    /** Re-reads the listing from scratch — the remediation behind every failure banner. */
    fun retry() {
        load()
    }

    /**
     * Moves to [index], clamped to the listing.
     *
     * Clamped rather than validated because the caller is a gesture: a fling past the end of
     * the listing is a normal thing for a finger to do, not a programming error.
     */
    fun openPage(index: Int) {
        val ready = mutableState.value as? ViewerUiState.Ready ?: return
        val target = index.coerceIn(0, ready.pages.size - 1)
        if (target == ready.index) return
        mutableState.value = ready.copy(index = target, representation = RepresentationStatus.Resolving)
        observeCurrentAsset(ready.pages[target].asset.id)
        ensureRepresentation(RepresentationTier.Preview)
    }

    /**
     * Fetches the original bytes for the open page.
     *
     * Only ever user-initiated. The original is never fetched speculatively — that frugality
     * is the download path's entire design — so wanting it is a decision the user makes.
     */
    fun loadFullResolution() {
        ensureRepresentation(RepresentationTier.Original)
    }

    /** Reads the listing, locates the asset the viewer was opened at, and starts observing it. */
    private fun load() {
        pageJob?.cancel()
        fetchJob?.cancel()
        mutableState.value = ViewerUiState.Loading
        viewModelScope.launch {
            // Only a `CapsuleError` is caught. A cancellation is not a failure to report — it
            // is this job being replaced — and swallowing it would leave a stale spinner where
            // the next load's state belongs.
            val listing =
                try {
                    readListing()
                } catch (error: CapsuleError) {
                    mutableState.value = ViewerUiState.Failed(ViewerFailure.of(error))
                    return@launch
                }
            val index = listing.indexOfFirst { it.id == openAt }
            if (index < 0) {
                mutableState.value = ViewerUiState.Missing
                return@launch
            }
            mutableState.value =
                ViewerUiState.Ready(
                    pages = listing.map(ViewerPage::of).toImmutableList(),
                    index = index,
                    representation = RepresentationStatus.Resolving,
                )
            observeCurrentAsset(openAt)
            ensureRepresentation(RepresentationTier.Preview)
        }
    }

    /**
     * The window the user can swipe through.
     *
     * Bounded rather than the whole library: a viewer is opened on one photo and swiped
     * through its neighbours, and holding a million records resident to support a gesture
     * nobody makes that far would cost more than the feature is worth. The window falls back
     * to the single asset when the listing does not contain it — a hidden or trashed asset
     * opened by deep link is still a legitimate thing to look at.
     */
    private suspend fun readListing(): List<Asset> {
        val listing =
            if (contextAlbum != null) {
                albums.albumAssets(contextAlbum, offset = 0, limit = WINDOW)
            } else {
                timeline.timelineWindow(offset = 0, limit = WINDOW)
            }
        if (listing.any { it.id == openAt }) return listing
        return listOfNotNull(assets.findByUuid(openAt))
    }

    /**
     * Keeps the open page's record live.
     *
     * Only the open page is observed. Re-deriving forty pages on every unrelated write would
     * turn a rating keystroke into a full-listing recomposition, and nothing off screen is
     * being looked at anyway.
     */
    private fun observeCurrentAsset(id: AssetId) {
        pageJob?.cancel()
        pageJob =
            viewModelScope.launch {
                assets.observeAsset(id).collect { asset ->
                    val ready = mutableState.value as? ViewerUiState.Ready ?: return@collect
                    if (ready.current.asset.id != id) return@collect
                    if (asset == null) {
                        mutableState.value = ViewerUiState.Missing
                        return@collect
                    }
                    val pages = ready.pages.toMutableList()
                    pages[ready.index] = ViewerPage.of(asset)
                    mutableState.value = ready.copy(pages = pages.toImmutableList())
                }
            }
    }

    /**
     * Asks for [wanted] on the open page and records where the ladder actually landed.
     *
     * The contract returns what is *in hand* rather than throwing on a permanent failure, so
     * a resolved tier below the wanted one is the degrade signal — no exception involved.
     * The two failures that do throw are kept apart deliberately: a pending upload is
     * transient state with its own badge, and anything else is a refusal the user can act on.
     */
    private fun ensureRepresentation(wanted: RepresentationTier) {
        val ready = mutableState.value as? ViewerUiState.Ready ?: return
        val id = ready.current.asset.id
        fetchJob?.cancel()
        fetchJob =
            viewModelScope.launch {
                val status =
                    try {
                        val resolved = sync.ensureRepresentation(id, wanted)
                        if (resolved.rank < wanted.rank) {
                            RepresentationStatus.Degraded(wanted = wanted, held = resolved)
                        } else {
                            RepresentationStatus.Held(resolved)
                        }
                    } catch (pending: BlobError.PendingUpload) {
                        // Caught ahead of the general arm and carried forward rather than
                        // dropped: `pending_upload` is explicitly not a `410`, so it becomes
                        // a badge that re-fetches when the feed flips rather than a failure
                        // the user is asked to act on.
                        RepresentationStatus.AwaitingOriginal(ViewerFailure.of(pending))
                    } catch (error: CapsuleError) {
                        RepresentationStatus.Refused(ViewerFailure.of(error))
                    }
                publishRepresentation(id, status)
            }
    }

    /** Applies a fetch outcome, ignoring one that arrived after the user swiped away. */
    private fun publishRepresentation(
        id: AssetId,
        status: RepresentationStatus,
    ) {
        val ready = mutableState.value as? ViewerUiState.Ready ?: return
        if (ready.current.asset.id != id) return
        mutableState.value =
            ready.copy(
                representation = status,
                originDeviceLabel = originLabelFor(ready.current.asset),
            )
    }

    /** The label for the device an asset was imported on, when the directory names one. */
    private fun originLabelFor(asset: Asset): String? = asset.importedByDevice?.let { deviceNames.value[it.value] }

    /**
     * Watches the signed device directory for display names.
     *
     * Failures are swallowed to an empty map on purpose. The directory is server-held state
     * and the awaiting-original badge has to keep working offline; a viewer that refused to
     * say "the original is on another device" because it could not say *which* would be worse
     * than one that says it plainly.
     */
    private suspend fun observeDeviceDirectory(sessions: SessionRepository) {
        sessions
            .devices()
            .catch { emit(emptyList()) }
            .collectLatest { directory ->
                deviceNames.value = directory.toLabelMap()
                // The badge may already be showing without a name — the directory is a
                // separate, slower read — so republish once the label becomes available.
                val ready = mutableState.value as? ViewerUiState.Ready ?: return@collectLatest
                mutableState.value = ready.copy(originDeviceLabel = originLabelFor(ready.current.asset))
            }
    }

    private companion object {
        /** How many neighbours the pager holds. Bounded by the gesture, not by the library. */
        const val WINDOW = 240
    }
}

/** Device ids to the label the directory carries, skipping the ones with none. */
private fun List<Device>.toLabelMap(): Map<String, String> =
    mapNotNull { device -> device.displayName?.let { device.id.value to it } }.toMap()
