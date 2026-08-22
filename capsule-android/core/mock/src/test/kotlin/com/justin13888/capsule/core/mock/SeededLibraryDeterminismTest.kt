package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.mock.seed.LibraryDigest
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The property the whole module rests on: one seed, one library.
 *
 * Asserted over a digest of every record rather than over a sample, because a spot check
 * passes happily while a camera serial four thousand assets in has drifted — and that drift
 * surfaces later as a screenshot diff nobody can explain.
 */
class SeededLibraryDeterminismTest {
    @Test
    fun `the same spec produces a byte-identical library twice`() {
        val first = SeededLibrary.generate(LibrarySpec.SMALL)
        val second = SeededLibrary.generate(LibrarySpec.SMALL)
        assertEquals(LibraryDigest.of(first), LibraryDigest.of(second))
    }

    @Test
    fun `the full-size spec is reproducible too`() {
        // The small spec exercises the same code paths, but only the full spec exercises them
        // forty thousand times — and an accumulator that drifts once every few thousand draws
        // would pass at two thousand assets and fail in the app.
        val first = SeededLibrary.generate()
        val second = SeededLibrary.generate()
        assertEquals(LibraryDigest.of(first), LibraryDigest.of(second))
        assertEquals(LibrarySpec.DEFAULT_ASSET_COUNT, first.assets.size)
    }

    @Test
    fun `a different seed produces a different library`() {
        val baseline = SeededLibrary.generate(LibrarySpec.SMALL)
        val other = SeededLibrary.generate(LibrarySpec.SMALL.copy(seed = LibrarySpec.SMALL.seed + 1))
        assertNotEquals(LibraryDigest.of(baseline), LibraryDigest.of(other))
    }

    @Test
    fun `the asset count is hit exactly, not approximately`() {
        // The allocator hands out the budget by cumulative rounding precisely so a fixture's
        // size does not wander with its seed; a fixture whose size wanders cannot be asserted.
        listOf(0L, 7L, 4_242L).forEach { seed ->
            val library = SeededLibrary.generate(LibrarySpec.SMALL.copy(seed = seed))
            assertEquals(LibrarySpec.SMALL.targetAssetCount, library.assets.size, "seed $seed")
        }
    }

    @Test
    fun `assets are ordered newest capture first`() {
        val captures = MockFixtures.LIBRARY.assets.map { it.captureTimestamp }
        assertTrue(captures.zipWithNext().all { (newer, older) -> !newer.isBefore(older) })
    }

    @Test
    fun `an empty spec still produces a usable library`() {
        // The first-run state is a legitimate fixture: the default container exists before
        // anything has been filed into it.
        val empty = SeededLibrary.generate(LibrarySpec.EMPTY)
        assertTrue(empty.assets.isEmpty())
        assertTrue(empty.albums.any { it.isDefault })
    }
}
