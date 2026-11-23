package com.justin13888.capsule.feature.search

import androidx.compose.runtime.Immutable
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.ImmutableSet
import kotlinx.collections.immutable.persistentSetOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.collections.immutable.toImmutableSet

/**
 * The nine dimensions a result set may be narrowed along.
 *
 * A closed set rather than an open list of strings, because the grouping rule below is
 * per-kind: values inside one kind are alternatives (`OR`), values across kinds are
 * requirements (`AND`). A caller that could invent a tenth kind could not be told which of
 * the two it obeys, and "photos rated four *and* five" would silently return nothing.
 *
 * The vocabulary is the smart-album grammar's, one facet per query field the user actually
 * reaches for — so a filter set here reads as the same question a saved search would ask.
 */
enum class FacetKind {
    /** The year the shot was taken. */
    Date,

    /** Still or moving image. */
    MediaKind,

    /** Stars, `0..5`. */
    Rating,

    /** The pick / neutral / reject review flag. */
    Cull,

    /** A tag, from either namespace — the label says which the value came from. */
    Tag,

    /** Someone the user has named. */
    Person,

    /** Whether a location is stored at all. */
    Place,

    /** The camera that produced the file. */
    Camera,

    /** The container album the asset lives in. */
    Album,
}

/**
 * One selectable point in the facet space: a kind, plus an id stable across reloads.
 *
 * The id is deliberately the underlying token — a wire value, a UUID, a tag's own text —
 * and never the display label. Selections survive a re-query and a locale change because
 * of it; keying on the label would drop every active filter the moment a translation
 * loaded.
 */
@Immutable
data class FacetRef(
    /** Which dimension this point belongs to. */
    val kind: FacetKind,
    /** The stable token identifying the point inside its dimension. */
    val id: String,
)

/**
 * The fixed vocabulary values a facet can take, as opposed to the ones the library supplies.
 *
 * Modelled as an enum rather than as a string resource so the view model never names an
 * Android resource: the state it publishes has to be renderable in a unit test with no
 * resources loaded, and translated once it reaches a composition.
 */
enum class FacetTerm {
    /** A still image. */
    Image,

    /** A moving image. */
    Video,

    /** No stars at all. */
    Unrated,

    /** Kept in a review pass. */
    Picked,

    /** Never flagged either way. */
    Unreviewed,

    /** Marked to throw away. */
    Rejected,

    /** Carries a stored fix. */
    Located,

    /** Carries no stored fix. */
    Unlocated,

    /** No camera identity was recorded. */
    NoCamera,

    /** Belongs to no album. */
    NoAlbum,

    /** The nameless album imports land in. */
    DefaultAlbum,
}

/** How a facet value spells itself once it reaches a composition. */
@Immutable
sealed interface FacetLabel {
    /**
     * Text the library supplied — a tag, a person's name, a camera model, a year.
     *
     * Never translated: it is the user's own content, or a manufacturer's product name.
     */
    @Immutable
    data class Data(
        val text: String,
    ) : FacetLabel

    /** One of the fixed vocabulary values, translated at render time. */
    @Immutable
    data class Term(
        val term: FacetTerm,
    ) : FacetLabel

    /** A star count, which reads as a number in a sentence rather than as a bare token. */
    @Immutable
    data class Stars(
        val stars: Int,
    ) : FacetLabel
}

/** One facet point with the label it should carry. */
@Immutable
data class FacetPoint(
    /** The point itself. */
    val ref: FacetRef,
    /** What to call it on screen. */
    val label: FacetLabel,
)

/** A facet point plus how many of the current hits sit on it. */
@Immutable
data class FacetValue(
    /** The point. */
    val point: FacetPoint,
    /** How many hits carry it. Zero-count points are never published. */
    val count: Int,
) {
    /** Shorthand for the point's ref. */
    val ref: FacetRef get() = point.ref
}

/** Every value one dimension offers over the current hits, most common first. */
@Immutable
data class FacetGroup(
    /** The dimension. */
    val kind: FacetKind,
    /** Its values, descending by count and then by label for a stable order. */
    val values: ImmutableList<FacetValue>,
)

/**
 * The filters currently switched on.
 *
 * A flat set rather than a map of kind to values: the set *is* the user's selection, and
 * every operation on it — toggle, count, clear, test — is a one-liner. Grouping happens
 * where grouping matters, inside [Faceting.apply].
 */
@Immutable
data class SearchFilters(
    /** The selected points. */
    val selected: ImmutableSet<FacetRef> = persistentSetOf(),
) {
    /** Whether nothing is filtered at all. */
    val isEmpty: Boolean get() = selected.isEmpty()

    /** How many points are on, for the badge beside the all-filters button. */
    val count: Int get() = selected.size

    /** Whether [ref] is on. */
    operator fun contains(ref: FacetRef): Boolean = ref in selected

    /** This selection with [ref] flipped. */
    fun toggled(ref: FacetRef): SearchFilters {
        val next = if (ref in selected) selected - ref else selected + ref
        return copy(selected = next.toImmutableSet())
    }

    /** This selection with everything off. */
    fun cleared(): SearchFilters = SearchFilters()
}

/**
 * One hit, with every facet point it sits on already worked out.
 *
 * The points are computed once, at assembly, rather than on every filter toggle. Toggling a
 * chip is the most latency-visible thing on this screen — the grid under it must not stall —
 * and re-deriving a thousand assets' tags, people and albums per tap would guarantee it does.
 */
@Immutable
data class SearchCandidate(
    /** What the grid draws. */
    val item: SearchResultItem,
    /** Every point this hit sits on. */
    val points: ImmutableList<FacetPoint>,
)

/**
 * Counting and filtering over a hit set.
 *
 * Pure, and separate from the view model, because the interesting behaviour is entirely in
 * these two rules and neither of them needs a coroutine, a repository, or a composition to
 * be checked.
 */
object Faceting {
    /**
     * Every dimension's values over [candidates], in the order the sheet lists them.
     *
     * Counts are over the *unfiltered* hits on purpose. A count that shrank as the user
     * selected would make each remaining chip look emptier than it is and turn narrowing
     * into guesswork.
     */
    fun groups(candidates: List<SearchCandidate>): List<FacetGroup> {
        val counts = LinkedHashMap<FacetRef, Int>()
        val labels = LinkedHashMap<FacetRef, FacetLabel>()
        candidates.forEach { candidate ->
            candidate.points.forEach { point ->
                counts[point.ref] = (counts[point.ref] ?: 0) + 1
                labels.putIfAbsent(point.ref, point.label)
            }
        }
        return FacetKind.entries.mapNotNull { kind ->
            val values =
                counts
                    .asSequence()
                    .filter { it.key.kind == kind }
                    .map { FacetValue(FacetPoint(it.key, labels.getValue(it.key)), it.value) }
                    .sortedWith(compareByDescending(FacetValue::count).thenBy { it.ref.id })
                    .toList()
            if (values.isEmpty()) null else FacetGroup(kind, values.toImmutableList())
        }
    }

    /**
     * The hits that survive [filters].
     *
     * Within a kind the selected values are alternatives; across kinds they are
     * requirements. That is the only reading that makes a facet UI usable: selecting a
     * second camera has to widen the answer, and selecting a camera *and* a year has to
     * narrow it.
     */
    fun apply(
        candidates: List<SearchCandidate>,
        filters: SearchFilters,
    ): List<SearchCandidate> {
        if (filters.isEmpty) return candidates
        val wanted = filters.selected.groupBy(FacetRef::kind)
        return candidates.filter { candidate ->
            val held = candidate.points.mapTo(HashSet(), FacetPoint::ref)
            wanted.values.all { alternatives -> alternatives.any { it in held } }
        }
    }

    /**
     * The handful of points to offer before the user has typed anything.
     *
     * Drawn from the dimensions a photographer starts from rather than from raw counts: the
     * most common value of every facet would be nine near-identical year chips, which
     * suggests nothing.
     */
    fun suggestions(
        groups: List<FacetGroup>,
        limit: Int = SUGGESTION_LIMIT,
    ): List<FacetValue> {
        val order = listOf(FacetKind.Person, FacetKind.MediaKind, FacetKind.Cull, FacetKind.Rating, FacetKind.Date)
        return order
            .mapNotNull { kind -> groups.firstOrNull { it.kind == kind }?.values?.firstOrNull() }
            .take(limit)
    }

    /** How many suggested facets the entry screen offers. Enough to start from, few enough to read. */
    const val SUGGESTION_LIMIT: Int = 5
}
