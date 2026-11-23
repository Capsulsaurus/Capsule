package com.justin13888.capsule.feature.search

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.MetadataRepository
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.data.PeopleRepository
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.data.SearchRepository
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.Asset
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.launchIn
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The search screen's state machine.
 *
 * Two things about the shape are deliberate.
 *
 * The hit window is fetched once per *query*, and every filter toggle is answered from it
 * without touching a repository. Faceting is a narrowing of an answer already in hand; going
 * back to the catalog per chip would put a round trip on the most-tapped control on screen
 * and would let the counts on the chips disagree with the grid beneath them.
 *
 * A blank query with filters on is a real search, not an empty one. It runs over the
 * timeline window instead of the text index, which is what makes the suggested facets on the
 * entry screen somewhere to start from rather than decoration.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SearchViewModel(
    private val search: SearchRepository,
    private val timeline: TimelineRepository,
    private val metadata: MetadataRepository,
    private val people: PeopleRepository,
    private val albums: AlbumRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(SearchUiState())

    /** Everything the screen renders. */
    val state: StateFlow<SearchUiState> = mutableState.asStateFlow()

    /**
     * The unfiltered hits behind the current answer.
     *
     * Held outside the published state on purpose: the screen never needs the points, only
     * the counts and the survivors, and publishing several hundred candidates would make
     * every filter toggle diff a list nothing renders.
     */
    private var candidates: List<SearchCandidate> = emptyList()

    private var loadJob: Job? = null

    init {
        observeCompletions()
        reload()
    }

    /**
     * The user typed.
     *
     * Completions move; the answer waits for a submit, because running a semantic query per
     * keystroke would make the grid flicker through half-typed nonsense. Emptying the box is
     * the exception: it is the clear button's whole purpose, so it puts the entry screen back
     * rather than leaving the previous answer sitting under a blank field.
     */
    fun onQueryChange(query: String) {
        val wasSearching = mutableState.value.query.isNotBlank()
        mutableState.update { it.copy(query = query) }
        if (query.isBlank() && wasSearching && mutableState.value.filters.isEmpty) reload()
    }

    /** The user asked for the typed query. */
    fun onSubmit() {
        val query = mutableState.value.query.trim()
        if (query.isNotEmpty()) mutableState.update { it.copy(recents = withRecent(it.recents, query)) }
        reload()
    }

    /** The user picked one of their recent searches. */
    fun onRecentSelected(query: String) {
        mutableState.update { it.copy(query = query) }
        onSubmit()
    }

    /** The user emptied the recent-search list. */
    fun onClearRecents() {
        mutableState.update { it.copy(recents = emptyList<String>().toImmutableList()) }
    }

    /** A filter chip was flipped. Answered from the hits already in hand. */
    fun onToggleFacet(ref: FacetRef) {
        mutableState.update { current -> publish(current.copy(filters = current.filters.toggled(ref))) }
    }

    /** Every filter off. */
    fun onClearFilters() {
        mutableState.update { current -> publish(current.copy(filters = current.filters.cleared())) }
    }

    /** The advanced filter sheet was opened or dismissed. */
    fun onFilterSheetOpenChange(open: Boolean) {
        mutableState.update { it.copy(filterSheetOpen = open) }
    }

    /** The remediation on the error state. */
    fun onRetry() {
        reload()
    }

    /**
     * Feeds the completion list from whatever is in the box.
     *
     * `flatMapLatest` rather than a debounce: the suggestion source is local and returns
     * immediately, so the only thing worth guaranteeing is that a slower answer for an older
     * prefix can never overwrite a newer one.
     */
    private fun observeCompletions() {
        mutableState
            .map { it.query }
            .distinctUntilChanged()
            .flatMapLatest { prefix -> search.suggestions(prefix) }
            .onEach { completions -> mutableState.update { it.copy(completions = completions.toImmutableList()) } }
            .launchIn(viewModelScope)
    }

    private fun reload() {
        loadJob?.cancel()
        loadJob =
            viewModelScope.launch {
                mutableState.update { it.copy(phase = SearchPhase.Loading, failure = null) }
                try {
                    val loaded = assemble(mutableState.value.query.trim())
                    candidates = loaded.candidates
                    mutableState.update { current -> publish(current.copy(staleModels = loaded.stale)) }
                } catch (error: CapsuleError) {
                    candidates = emptyList()
                    mutableState.update {
                        it.copy(phase = SearchPhase.Failed, failure = failureOf(error), results = emptyResults())
                    }
                }
            }
    }

    /** One window of hits, plus what the query was not allowed to see. */
    private data class Loaded(
        val candidates: List<SearchCandidate>,
        val stale: StaleModelNotice?,
    )

    private suspend fun assemble(query: String): Loaded {
        val hits = if (query.isEmpty()) timeline.timelineWindow(0, WINDOW) else search.search(query, 0, WINDOW)
        // Sorted here rather than trusted from the repository: the grid's year sections
        // require a monotone order, and a section boundary computed over an unordered list
        // would repeat the same year three headers apart.
        val ordered = hits.sortedWith(compareByDescending(Asset::captureTimestamp).thenBy { it.id.value })
        val tags = ordered.associate { it.id to metadata.tags(it.id).first() }
        val clusters = people.clusters().first()
        val named = people.people().first()
        return Loaded(
            candidates =
                SearchIndexing.candidates(
                    assets = ordered,
                    tags = tags,
                    peopleByAsset = SearchIndexing.peopleIndex(named, clusters),
                    albums = albums.albums().first(),
                ),
            stale = SearchIndexing.staleNotice(clusters, tags.values),
        )
    }

    /** Re-derives everything that follows from the hits and the filters. */
    private fun publish(current: SearchUiState): SearchUiState {
        val groups = Faceting.groups(candidates)
        val surviving = Faceting.apply(candidates, current.filters).map(SearchCandidate::item)
        return current.copy(
            phase = phaseOf(current, surviving),
            groups = groups.toImmutableList(),
            suggested = Faceting.suggestions(groups).toImmutableList(),
            results = surviving.toImmutableList(),
            hitCount = candidates.size,
            failure = null,
        )
    }

    private fun phaseOf(
        current: SearchUiState,
        surviving: List<SearchResultItem>,
    ): SearchPhase =
        when {
            current.query.isBlank() && current.filters.isEmpty -> SearchPhase.Entry
            surviving.isEmpty() -> SearchPhase.NoResults
            else -> SearchPhase.Results
        }

    private fun withRecent(
        recents: List<String>,
        query: String,
    ) = (listOf(query) + recents.filterNot { it.equals(query, ignoreCase = true) })
        .take(MAX_RECENTS)
        .toImmutableList()

    private fun emptyResults() = emptyList<SearchResultItem>().toImmutableList()

    /**
     * Resolves a repository failure to something the screen can say out loud.
     *
     * The `error.*` code is collapsed here rather than published, so the screen's `when` is
     * exhaustive and no unrecognised code can reach a user as a raw token.
     */
    private fun failureOf(error: CapsuleError): SearchFailure =
        when (error) {
            is LocalError.Offline -> SearchFailure.Offline
            is ModerationError.AccountSuspended -> SearchFailure.Suspended
            is QuotaError.GraceLocked, is QuotaError.Exceeded -> SearchFailure.QuotaLocked
            is ProtocolError.VersionUnsupported -> SearchFailure.Unsupported
            else -> SearchFailure.Unknown
        }

    private companion object {
        /**
         * How many hits one query fetches.
         *
         * Bounded because everything downstream — the tag read per hit, the facet counts, the
         * solved layout — is linear in it, and because a facet row counted over ten thousand
         * assets is not more useful than one counted over a few hundred.
         */
        const val WINDOW = 240

        /** How many past queries the entry screen offers. */
        const val MAX_RECENTS = 8
    }
}
