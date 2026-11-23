package com.justin13888.capsule.feature.search

import kotlinx.collections.immutable.persistentSetOf
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The facet algebra.
 *
 * These two rules are the whole of what a faceted search means, and getting either backwards
 * produces a UI that looks right and answers the wrong question: `OR` across kinds would make
 * every extra filter widen the answer, and `AND` within a kind would make "photos or videos"
 * return nothing at all.
 */
class SearchFacetsTest {
    private val nikon = pointOf(FacetKind.Camera, "Nikon Z8")
    private val fuji = pointOf(FacetKind.Camera, "Fujifilm X-T5")
    private val yearOld = pointOf(FacetKind.Date, "2024")
    private val yearNew = pointOf(FacetKind.Date, "2026")

    private val candidates =
        listOf(
            candidateOf("a", nikon, yearNew),
            candidateOf("b", fuji, yearNew),
            candidateOf("c", fuji, yearOld),
        )

    @Test
    fun `values inside one kind are alternatives`() {
        val filters = SearchFilters(persistentSetOf(nikon.ref, fuji.ref))

        val surviving = Faceting.apply(candidates, filters).map { it.item.assetId }

        assertEquals(listOf("a", "b", "c"), surviving)
    }

    @Test
    fun `values across kinds are requirements`() {
        val filters = SearchFilters(persistentSetOf(fuji.ref, yearOld.ref))

        val surviving = Faceting.apply(candidates, filters).map { it.item.assetId }

        assertEquals(listOf("c"), surviving)
    }

    @Test
    fun `an empty selection filters nothing`() {
        assertEquals(candidates, Faceting.apply(candidates, SearchFilters()))
    }

    @Test
    fun `groups count every candidate and order by count`() {
        val groups = Faceting.groups(candidates)

        val cameras = groups.single { it.kind == FacetKind.Camera }
        assertEquals(listOf("Fujifilm X-T5", "Nikon Z8"), cameras.values.map { it.ref.id })
        assertEquals(listOf(2, 1), cameras.values.map(FacetValue::count))
    }

    @Test
    fun `groups omit a kind nothing sits on`() {
        val groups = Faceting.groups(candidates)

        assertTrue(groups.none { it.kind == FacetKind.Person })
    }

    @Test
    fun `toggling is its own inverse`() {
        val once = SearchFilters().toggled(fuji.ref)
        val twice = once.toggled(fuji.ref)

        assertEquals(1, once.count)
        assertTrue(twice.isEmpty)
    }

    @Test
    fun `suggestions offer one value per dimension a photographer starts from`() {
        val withPeople =
            candidates + candidateOf("d", pointOf(FacetKind.Person, "person-1"), yearNew)

        val suggested = Faceting.suggestions(Faceting.groups(withPeople))

        assertEquals(FacetKind.Person, suggested.first().ref.kind)
        assertTrue(suggested.size <= Faceting.SUGGESTION_LIMIT)
        assertEquals(suggested.map { it.ref.kind }.distinct(), suggested.map { it.ref.kind })
    }
}
