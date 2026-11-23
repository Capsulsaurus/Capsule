package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.model.MapBounds
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Clustering fixes into pins, and putting the pins where they belong.
 *
 * The property worth defending hardest is that two datums never share a bucket: coordinates
 * are stored verbatim in whatever datum their source supplied and are never converted at
 * rest, so averaging a WGS-84 fix with a GCJ-02 one would place a pin a few hundred metres
 * from either photograph.
 */
class PlaceClusteringTest {
    private fun located(
        id: String,
        latitude: Double,
        longitude: Double,
        datum: GpsDatum = GpsDatum.Wgs84,
    ) = AlbumsFixtures
        .asset(id)
        .copy(gps = GpsFix(latitude, longitude, GpsSource.Exif.asWire(), datum.asWire()))

    @Test
    fun `nearby fixes share one pin`() {
        val clusters =
            PlaceClustering.cluster(listOf(located("a", 49.30, -123.10), located("b", 49.31, -123.11)))
        assertEquals(1, clusters.size)
        assertEquals(2, clusters.single().count)
    }

    @Test
    fun `distant fixes get their own pins, largest first`() {
        val clusters =
            PlaceClustering.cluster(
                listOf(
                    located("a", 49.30, -123.10),
                    located("b", 49.31, -123.11),
                    located("c", 35.68, 139.69),
                ),
            )
        assertEquals(listOf(2, 1), clusters.map { it.count })
    }

    @Test
    fun `fixes in different datums are never averaged together`() {
        val clusters =
            PlaceClustering.cluster(
                listOf(
                    located("a", 39.90, 116.39),
                    located("b", 39.90, 116.39, datum = GpsDatum.Gcj02),
                ),
            )
        assertEquals(2, clusters.size)
        assertEquals(setOf(GpsDatum.Wgs84, GpsDatum.Gcj02), clusters.map { it.datum }.toSet())
    }

    @Test
    fun `an asset without a fix contributes no pin`() {
        assertTrue(PlaceClustering.cluster(listOf(AlbumsFixtures.asset("a"))).isEmpty())
    }

    @Test
    fun `projection puts the north-west corner at the top left`() {
        val clusters = PlaceClustering.cluster(listOf(located("a", 50.0, -10.0), located("b", 40.0, 10.0)))
        val bounds = requireNotNull(MapBounds.around(clusters))
        val north = clusters.first { it.latitude > 45 }
        val (x, y) = PlaceClustering.project(north, bounds)
        assertEquals(0f, x)
        assertEquals(0f, y)
    }

    @Test
    fun `a single pin still gets a window wide enough to draw`() {
        val clusters = PlaceClustering.cluster(listOf(located("a", 49.30, -123.10)))
        val bounds = requireNotNull(MapBounds.around(clusters))
        // Compared with a tolerance: the padding is centred arithmetic on doubles, so the
        // span lands within a rounding step of the minimum rather than exactly on it.
        assertEquals(MapBounds.MIN_SPAN, bounds.maxLatitude - bounds.minLatitude, TOLERANCE)
        assertEquals(MapBounds.MIN_SPAN, bounds.maxLongitude - bounds.minLongitude, TOLERANCE)
    }

    private companion object {
        /** One rounding step at the scale a degree span is compared at. */
        const val TOLERANCE = 1e-9
    }

    @Test
    fun `no pins means no window`() {
        assertNull(MapBounds.around(emptyList()))
    }
}
