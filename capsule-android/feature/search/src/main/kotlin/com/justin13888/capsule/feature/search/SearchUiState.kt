package com.justin13888.capsule.feature.search

import androidx.compose.runtime.Immutable
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf

/**
 * One hit, reduced to exactly what the grid draws.
 *
 * A projection rather than the `Asset` itself. Compose cannot prove a domain record with
 * list-typed fields is stable, so passing one into a cell would defeat skipping across the
 * whole viewport; and the grid genuinely needs four numbers and an id, not a catalog row.
 */
@Immutable
data class SearchResultItem(
    /** The asset's id, as the shell's navigation keys carry it. */
    val assetId: String,
    /** Width over height. The only thing the layout engine consumes. */
    val aspectRatio: Float,
    /** The capture year, which is also the grid's section key. */
    val year: Int,
    /** `0xRRGGBB` of the asset's dominant colour, drawn until a thumbnail exists. */
    val placeholder: Int,
    /** An already-formatted running time for a video, or `null` for a still. */
    val videoDuration: String? = null,
)

/**
 * What the search screen is currently doing.
 *
 * Five states rather than the usual three: "nothing typed yet" and "typed, and nothing
 * matched" are different screens with different exits, and collapsing them produces the
 * classic bug where a first-time user is told their library is empty.
 */
enum class SearchPhase {
    /** Nothing has been asked yet. Recents and suggested facets are the content. */
    Entry,

    /** A query or a filter change is in flight. */
    Loading,

    /** There are results to show. */
    Results,

    /** The query ran and nothing survived it. */
    NoResults,

    /** The query could not run at all. */
    Failed,
}

/**
 * Why a search could not run, in terms the screen can localize.
 *
 * The view model resolves the repository's `error.*` code down to one of these rather than
 * publishing the code itself, so that the screen has an exhaustive `when` and no path where
 * an unrecognised code reaches the user as a raw token.
 */
enum class SearchFailure {
    /** No usable network path. Search itself is local, so this only reaches the facets. */
    Offline,

    /** The account is administratively suspended. */
    Suspended,

    /** Past the quota grace window: metadata growth is refused. */
    QuotaLocked,

    /** The library holds state this build cannot read. */
    Unsupported,

    /** Anything the catalogue does not name. */
    Unknown,
}

/**
 * The AI outputs the current query is deliberately not seeing.
 *
 * Its presence on the state is the whole point: when a model version changes, its outputs
 * are excluded from queries until they are regenerated, and a screen that simply returned
 * fewer results would be lying by omission. See the AI design doc's embedding-provenance
 * rule — stale-but-known outputs are kept as the regeneration queue, not discarded.
 */
@Immutable
data class StaleModelNotice(
    /** How many AI-derived values are being held out of the answer. */
    val excludedCount: Int,
    /** The superseded `model_id model_version` slots behind them, for the detail line. */
    val slots: ImmutableList<String>,
)

/**
 * Everything the search screen renders, as one immutable value.
 *
 * One state object rather than a handful of flows because every field here is read together
 * on one frame: a chip row that recomposed against a newer filter set than the grid beneath
 * it would show counts that do not match the photographs.
 */
@Immutable
data class SearchUiState(
    /** What is in the search box. */
    val query: String = "",
    /** Which screen to draw. */
    val phase: SearchPhase = SearchPhase.Loading,
    /** The most recent queries this session ran, newest first. */
    val recents: ImmutableList<String> = persistentListOf(),
    /** Completions for the partly typed query. */
    val completions: ImmutableList<String> = persistentListOf(),
    /** The facets offered before anything has been asked. */
    val suggested: ImmutableList<FacetValue> = persistentListOf(),
    /** The filters currently on. */
    val filters: SearchFilters = SearchFilters(),
    /** Every dimension of the current hits, for the chip row and the sheet. */
    val groups: ImmutableList<FacetGroup> = persistentListOf(),
    /** The hits that survive [filters], newest capture first. */
    val results: ImmutableList<SearchResultItem> = persistentListOf(),
    /** How many hits there were before [filters] narrowed them. */
    val hitCount: Int = 0,
    /** The held-back AI outputs, or `null` when every model slot is current. */
    val staleModels: StaleModelNotice? = null,
    /** Why the last attempt failed, or `null`. */
    val failure: SearchFailure? = null,
    /** Whether the advanced filter sheet is up. */
    val filterSheetOpen: Boolean = false,
) {
    /** Whether the entry screen has anything at all to offer — recents or suggestions. */
    val hasEntryContent: Boolean get() = recents.isNotEmpty() || suggested.isNotEmpty()
}
