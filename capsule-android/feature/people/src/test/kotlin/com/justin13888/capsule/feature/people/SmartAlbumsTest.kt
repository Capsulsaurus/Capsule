package com.justin13888.capsule.feature.people

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Duration
import java.time.Instant

/**
 * The AI smart albums.
 *
 * The three rules under test are the ones a careless implementation gets wrong in a way
 * nobody notices for months: an album must read only the AI namespace, it must ignore outputs
 * from a model version that has been replaced, and its membership must be a pure function of
 * the library rather than anything stored.
 */
class SmartAlbumsTest {
    private val base = Instant.parse("2026-03-01T09:00:00Z")

    @Test
    fun `a user tag never puts a photograph in an automatic album`() {
        val asset = testAsset("a")

        val food = SmartAlbums.tagged(listOf(asset), mapOf(asset.id to userTags("food")), setOf("food"))

        assertTrue(food.isEmpty(), "a user-written tag was read by an AI-built album")
    }

    @Test
    fun `a suggestion from a replaced model version does not count`() {
        val asset = testAsset("a")

        val pets = SmartAlbums.tagged(listOf(asset), mapOf(asset.id to staleAiTags("dog")), setOf("dog"))

        assertTrue(pets.isEmpty(), "an output from a superseded model slot was queried")
    }

    @Test
    fun `a current suggestion does count`() {
        val asset = testAsset("a")

        val pets = SmartAlbums.tagged(listOf(asset), mapOf(asset.id to freshAiTags("dog")), setOf("dog"))

        assertEquals(listOf(asset.id), pets.map(Asset::id))
    }

    @Test
    fun `the built set is the same five albums whether or not any of them match`() {
        val built = SmartAlbums.build(emptyList(), emptyMap())

        assertEquals(SmartAlbumKind.entries.toList(), built.map { it.kind })
        assertTrue(built.all { it.assets.isEmpty() })
    }

    @Test
    fun `a run of located captures with quiet either side is a trip`() {
        val trip =
            (0 until SmartAlbums.MIN_TRIP_ASSETS).map {
                testAsset("t$it", base.plusSeconds(HOUR * it), located = true)
            }
        val stray = testAsset("stray", base.plus(Duration.ofDays(30)), located = true)

        val trips = SmartAlbums.trips(trip + stray).map { it.id.value }

        assertEquals(trip.map { it.id.value }.toSet(), trips.toSet())
    }

    @Test
    fun `too few located captures together are an errand, not a trip`() {
        val short =
            (0 until SmartAlbums.MIN_TRIP_ASSETS - 1).map {
                testAsset("s$it", base.plusSeconds(HOUR * it), located = true)
            }

        assertTrue(SmartAlbums.trips(short).isEmpty())
    }

    @Test
    fun `a photograph with no fix is never on a trip`() {
        val unlocated =
            (0 until SmartAlbums.MIN_TRIP_ASSETS + 2).map { testAsset("u$it", base.plusSeconds(HOUR * it)) }

        assertTrue(SmartAlbums.trips(unlocated).isEmpty())
    }

    @Test
    fun `best of the year is the most recent year present, not the calendar year`() {
        val old = testAsset("old", Instant.parse("2020-05-05T00:00:00Z"), rating = 5)
        val recent = testAsset("recent", Instant.parse("2024-05-05T00:00:00Z"), rating = 5)
        val unrated = testAsset("unrated", Instant.parse("2024-06-06T00:00:00Z"), rating = 2)

        val best = SmartAlbums.bestOfYear(listOf(old, recent, unrated)).map { it.id.value }

        assertEquals(listOf("recent"), best)
    }

    @Test
    fun `only the tag-driven albums declare a vocabulary`() {
        assertTrue(SmartAlbums.vocabularyOf(SmartAlbumKind.Pets).isNotEmpty())
        assertTrue(SmartAlbums.vocabularyOf(SmartAlbumKind.Food).isNotEmpty())
        assertTrue(SmartAlbums.vocabularyOf(SmartAlbumKind.Nature).isNotEmpty())
        assertTrue(SmartAlbums.vocabularyOf(SmartAlbumKind.Trips).isEmpty())
        assertTrue(SmartAlbums.vocabularyOf(SmartAlbumKind.BestOfYear).isEmpty())
    }

    @Test
    fun `membership is recomputed from the library, so two runs agree exactly`() {
        val assets = listOf(testAsset("a", base, located = true), testAsset("b", base.plusSeconds(60), rating = 5))
        val tags: Map<AssetId, AssetTags> = mapOf(assets[1].id to freshAiTags("mountain"))

        val first = SmartAlbums.build(assets, tags)
        val second = SmartAlbums.build(assets, tags)

        assertEquals(first, second)
    }

    private companion object {
        /** An hour, in seconds — comfortably inside the trip gap. */
        const val HOUR = 3_600L
    }
}
