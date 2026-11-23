package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.RgbColor
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.model.DuplicateKind
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Instant

/**
 * The duplicate finder, held to being conservative.
 *
 * The screen this feeds offers to move copies to the trash, so an over-grouping finder turns
 * one wrong guess into a batch of soft deletes. Two properties matter most: an asset with
 * nothing to compare is never grouped, and the suggested keeper is chosen by a total order so
 * a rescan of the same library makes the same suggestion.
 */
class DuplicateFinderTest {
    private val now: Instant = Instant.parse("2026-06-01T12:00:00Z")

    @Test
    fun `equal content hashes group as identical bytes`() {
        val groups =
            DuplicateFinder.find(
                listOf(
                    AlbumsFixtures.asset("a", hash = "same"),
                    AlbumsFixtures.asset("b", hash = "same"),
                    AlbumsFixtures.asset("c", hash = "other"),
                ),
            )
        val group = groups.single()
        assertEquals(DuplicateKind.Identical, group.kind)
        assertEquals(listOf("a", "b"), group.members.map { it.id.value })
    }

    @Test
    fun `same frame at different bytes groups as nearly identical`() {
        val groups =
            DuplicateFinder.find(
                listOf(
                    AlbumsFixtures.asset("a", hash = "one", capturedAt = now),
                    AlbumsFixtures.asset("b", hash = "two", capturedAt = now),
                ),
            )
        assertEquals(DuplicateKind.Similar, groups.single().kind)
    }

    @Test
    fun `a different frame is not grouped`() {
        val groups =
            DuplicateFinder.find(
                listOf(
                    AlbumsFixtures.asset("a", hash = "one", capturedAt = now),
                    AlbumsFixtures.asset("b", hash = "two", capturedAt = now, colour = RgbColor(9, 9, 9)),
                ),
            )
        assertTrue(groups.isEmpty())
    }

    @Test
    fun `captures far apart are not grouped even at the same size and colour`() {
        val groups =
            DuplicateFinder.find(
                listOf(
                    AlbumsFixtures.asset("a", hash = "one", capturedAt = now),
                    AlbumsFixtures.asset("b", hash = "two", capturedAt = now.plusSeconds(600)),
                ),
            )
        assertTrue(groups.isEmpty())
    }

    @Test
    fun `an asset with nothing to compare is left alone`() {
        val groups =
            DuplicateFinder.find(
                listOf(
                    AlbumsFixtures.asset("a", hash = "one", capturedAt = now, colour = null),
                    AlbumsFixtures.asset("b", hash = "two", capturedAt = now, colour = null),
                ),
            )
        assertTrue(groups.isEmpty())
    }

    @Test
    fun `the keeper is the copy this device holds most of`() {
        val thin = AlbumsFixtures.asset("thin", hash = "same")
        val full =
            AlbumsFixtures
                .asset("full", hash = "same")
                .copy(
                    heldRepresentations =
                        HeldRepresentations(setOf(RepresentationTier.Lqip, RepresentationTier.Original)),
                )
        val group = DuplicateFinder.find(listOf(thin, full)).single()
        assertEquals("full", group.keeper.id.value)
        assertEquals(listOf("thin"), group.others.map { it.id.value })
    }

    @Test
    fun `the same library produces the same suggestion on a rescan`() {
        val assets =
            listOf(
                AlbumsFixtures.asset("b", hash = "same", importedAt = now),
                AlbumsFixtures.asset("a", hash = "same", importedAt = now),
            )
        assertEquals(
            DuplicateFinder.find(assets).map { it.members.map { asset -> asset.id.value } },
            DuplicateFinder.find(assets.reversed()).map { it.members.map { asset -> asset.id.value } },
        )
    }
}
