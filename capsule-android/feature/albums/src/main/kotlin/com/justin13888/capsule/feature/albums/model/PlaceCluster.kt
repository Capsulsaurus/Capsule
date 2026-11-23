package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.GpsDatum

/**
 * A group of photographs that were taken close enough together to be one pin.
 *
 * [datum] travels on the cluster because coordinates are stored **verbatim in the datum
 * their source supplied** and are never converted at rest. A cluster of GCJ-02 fixes drawn
 * on a WGS-84 map is off by a few hundred metres, and the honest thing is to say so rather
 * than to silently convert — the inverse is not exact, so converting would destroy the
 * user's ground truth.
 */
data class PlaceCluster(
    /** Mean latitude of the members, in [datum]. */
    val latitude: Double,
    /** Mean longitude of the members, in [datum]. */
    val longitude: Double,
    /** The datum every member's fix is expressed in. */
    val datum: GpsDatum,
    /** The members, in the order the library listed them. */
    val assetIds: List<AssetId>,
) {
    /** How many photographs the pin stands for. */
    val count: Int get() = assetIds.size
}

/**
 * The geographic window a map is drawing, in degrees.
 *
 * Held as an explicit value rather than derived inside the canvas so the projection is a
 * pure function a test can pin down: a map whose maths only exists inside a draw scope is a
 * map whose pins can only be checked by looking at it.
 */
data class MapBounds(
    val minLatitude: Double,
    val maxLatitude: Double,
    val minLongitude: Double,
    val maxLongitude: Double,
) {
    /** Whether the window has any extent at all; a degenerate one is padded before use. */
    val isDegenerate: Boolean
        get() = maxLatitude - minLatitude < MIN_SPAN || maxLongitude - minLongitude < MIN_SPAN

    /** This window widened to at least [MIN_SPAN] on both axes, centred where it was. */
    fun padded(): MapBounds {
        if (!isDegenerate) return this
        val latitudeCentre = (minLatitude + maxLatitude) / 2
        val longitudeCentre = (minLongitude + maxLongitude) / 2
        val half = MIN_SPAN / 2
        return MapBounds(
            minLatitude = latitudeCentre - half,
            maxLatitude = latitudeCentre + half,
            minLongitude = longitudeCentre - half,
            maxLongitude = longitudeCentre + half,
        )
    }

    companion object {
        /** The narrowest window the projection will draw — roughly a kilometre. */
        const val MIN_SPAN = 0.01

        /** The window that contains every cluster, or `null` when there are none. */
        fun around(clusters: List<PlaceCluster>): MapBounds? {
            if (clusters.isEmpty()) return null
            return MapBounds(
                minLatitude = clusters.minOf(PlaceCluster::latitude),
                maxLatitude = clusters.maxOf(PlaceCluster::latitude),
                minLongitude = clusters.minOf(PlaceCluster::longitude),
                maxLongitude = clusters.maxOf(PlaceCluster::longitude),
            ).padded()
        }
    }
}
