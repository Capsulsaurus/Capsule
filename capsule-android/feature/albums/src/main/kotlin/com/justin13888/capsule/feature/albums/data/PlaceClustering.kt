package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.albums.model.MapBounds
import com.justin13888.capsule.feature.albums.model.PlaceCluster
import kotlin.math.floor

/**
 * Buckets stored fixes into map pins.
 *
 * A fixed-size lat/long grid rather than a distance-based clustering, for one reason that
 * outweighs the prettier pins: it is deterministic and order-independent, so the same
 * library produces the same pins on every device and every run. Anything greedy would let
 * the order the catalog happened to return assets in decide where the pins landed.
 *
 * Fixes in different datums never share a bucket. Merging them would average two coordinate
 * systems together, which is exactly the few-hundred-metre error the "store verbatim, never
 * convert at rest" rule exists to avoid.
 */
object PlaceClustering {
    /** Grid cell size in degrees — a little under ten kilometres at the equator. */
    const val CELL_DEGREES = 0.08

    /** Groups every located asset in [assets] into pins, largest first. */
    fun cluster(
        assets: List<Asset>,
        cellDegrees: Double = CELL_DEGREES,
    ): List<PlaceCluster> =
        assets
            .mapNotNull { asset -> asset.gps?.let { asset to it } }
            .groupBy { (_, fix) ->
                val datum = fix.datum.knownOrNull ?: GpsDatum.Wgs84
                Triple(datum, cell(fix.latitude, cellDegrees), cell(fix.longitude, cellDegrees))
            }.map { (key, entries) ->
                PlaceCluster(
                    latitude = entries.sumOf { it.second.latitude } / entries.size,
                    longitude = entries.sumOf { it.second.longitude } / entries.size,
                    datum = key.first,
                    assetIds = entries.map { it.first.id },
                )
            }
            // Largest pin first, then by position, so the order never depends on hashing.
            .sortedWith(
                compareByDescending(PlaceCluster::count)
                    .thenBy(PlaceCluster::latitude)
                    .thenBy(PlaceCluster::longitude),
            )

    /** Where a cluster sits inside [bounds], as a `0..1` pair of `(x, y)` fractions. */
    fun project(
        cluster: PlaceCluster,
        bounds: MapBounds,
    ): Pair<Float, Float> {
        val x = (cluster.longitude - bounds.minLongitude) / (bounds.maxLongitude - bounds.minLongitude)
        // Latitude grows northward and the canvas grows downward, so the axis is flipped
        // here rather than at every draw site.
        val y = 1.0 - (cluster.latitude - bounds.minLatitude) / (bounds.maxLatitude - bounds.minLatitude)
        return x.toFloat().coerceIn(0f, 1f) to y.toFloat().coerceIn(0f, 1f)
    }

    private fun cell(
        degrees: Double,
        cellDegrees: Double,
    ): Long = floor(degrees / cellDegrees).toLong()
}
