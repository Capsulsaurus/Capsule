package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.asWire

/** One vertex of a route, in the route's own datum. */
data class GeoPoint(
    /** Degrees north. */
    val latitude: Double,
    /** Degrees east. */
    val longitude: Double,
)

/**
 * A named place and the path taken through it.
 *
 * Coordinates follow a real road or a real walking line rather than being scattered inside a
 * bounding box, because the two look nothing alike once plotted. Uniform noise over a city
 * produces points in the middle of rivers and inside blocks with no streets; walking a
 * polyline produces a track that reads as a day out, which is what a map view in the app has
 * to be designed against.
 *
 * [datum] travels with the route, not with the fix's consumer: the Shanghai route is
 * [GpsDatum.Gcj02] because that is the datum a device records there, and the library stores
 * what the source supplied rather than converting at rest.
 */
data class GeoRoute(
    /** A human-readable place name, used to name the album a trip produces. */
    val name: String,
    /** The IANA zone the place sits in. */
    val timeZoneId: String,
    /** The route's offset from UTC in seconds, for the period the library covers. */
    val offsetSeconds: Int,
    /** The datum the vertices are expressed in. */
    val datum: GpsDatum,
    /** The path, in order. */
    val vertices: List<GeoPoint>,
) {
    /**
     * The point [fraction] of the way along the route, with [jitterMetres] of seeded scatter.
     *
     * Interpolation is by vertex index rather than by true arc length. That is a deliberate
     * simplification: arc-length parameterisation would need a haversine sum per lookup and
     * would move the points by tens of metres at most, which no map view at photo-library
     * zoom can resolve.
     */
    fun sample(
        fraction: Double,
        jitterMetres: Double,
        prng: Prng,
    ): GpsFix {
        val clamped = fraction.coerceIn(0.0, 1.0)
        val span = (vertices.size - 1).coerceAtLeast(1)
        val position = clamped * span
        val index = position.toInt().coerceAtMost(vertices.size - 2)
        val within = position - index
        val from = vertices[index]
        val to = vertices[index + 1]
        val latitude = from.latitude + (to.latitude - from.latitude) * within
        val longitude = from.longitude + (to.longitude - from.longitude) * within
        val latitudeJitter = (prng.nextDouble() - HALF) * 2 * jitterMetres / METRES_PER_DEGREE_LATITUDE
        val longitudeScale = METRES_PER_DEGREE_LATITUDE * kotlin.math.cos(Math.toRadians(latitude))
        val longitudeJitter = (prng.nextDouble() - HALF) * 2 * jitterMetres / longitudeScale
        return GpsFix(
            latitude = latitude + latitudeJitter,
            longitude = longitude + longitudeJitter,
            source = GpsSource.Exif.asWire(),
            datum = datum.asWire(),
        )
    }

    private companion object {
        const val HALF = 0.5

        /** One degree of latitude is very close to this many metres everywhere on Earth. */
        const val METRES_PER_DEGREE_LATITUDE = 111_320.0
    }
}

/**
 * The places the generated library was shot in.
 *
 * A small, fixed set rather than procedurally generated geography: the point of the routes is
 * that they are recognisable, so a map view drawn against them can be judged by eye. Every
 * route below traces a road or a walking line that exists.
 */
object GeoRoutes {
    /** Reykjavík out along the south coast — the Ring Road as far as Vík. */
    val ICELAND_SOUTH =
        GeoRoute(
            name = "Iceland",
            timeZoneId = "Atlantic/Reykjavik",
            offsetSeconds = 0,
            datum = GpsDatum.Wgs84,
            vertices =
                listOf(
                    GeoPoint(64.1466, -21.9426),
                    GeoPoint(64.0400, -21.1900),
                    GeoPoint(63.9330, -20.9750),
                    GeoPoint(63.6300, -19.9930),
                    GeoPoint(63.5320, -19.5110),
                    GeoPoint(63.4187, -19.0060),
                    GeoPoint(63.4058, -18.9970),
                ),
        )

    /** A day walking Kyoto, Gion to Fushimi Inari. */
    val KYOTO_WALK =
        GeoRoute(
            name = "Kyoto",
            timeZoneId = "Asia/Tokyo",
            offsetSeconds = 9 * SECONDS_PER_HOUR,
            datum = GpsDatum.Wgs84,
            vertices =
                listOf(
                    GeoPoint(35.0116, 135.7681),
                    GeoPoint(35.0037, 135.7788),
                    GeoPoint(34.9977, 135.7850),
                    GeoPoint(34.9948, 135.7853),
                    GeoPoint(34.9871, 135.7727),
                    GeoPoint(34.9671, 135.7727),
                ),
        )

    /** Manhattan, downtown along Broadway. */
    val MANHATTAN =
        GeoRoute(
            name = "New York",
            timeZoneId = "America/New_York",
            offsetSeconds = -4 * SECONDS_PER_HOUR,
            datum = GpsDatum.Wgs84,
            vertices =
                listOf(
                    GeoPoint(40.8075, -73.9626),
                    GeoPoint(40.7829, -73.9654),
                    GeoPoint(40.7614, -73.9776),
                    GeoPoint(40.7484, -73.9857),
                    GeoPoint(40.7411, -73.9897),
                    GeoPoint(40.7223, -73.9987),
                    GeoPoint(40.7061, -74.0087),
                ),
        )

    /** The Bernese Oberland, Interlaken up to Grindelwald and the Kleine Scheidegg. */
    val BERNESE_OBERLAND =
        GeoRoute(
            name = "Berner Oberland",
            timeZoneId = "Europe/Zurich",
            offsetSeconds = 2 * SECONDS_PER_HOUR,
            datum = GpsDatum.Wgs84,
            vertices =
                listOf(
                    GeoPoint(46.6863, 7.8632),
                    GeoPoint(46.6580, 7.8530),
                    GeoPoint(46.6244, 7.9800),
                    GeoPoint(46.6180, 8.0374),
                    GeoPoint(46.5852, 7.9615),
                    GeoPoint(46.5477, 7.9855),
                ),
        )

    /**
     * The Bund and People's Square, in Shanghai.
     *
     * The library's [GpsDatum.Gcj02] case. Chinese devices record the obfuscated datum and
     * the design stores exactly what the source supplied, so a client that assumed WGS-84
     * everywhere would plot this track a few hundred metres into the Huangpu — which is
     * precisely the bug this route exists to make visible.
     */
    val SHANGHAI_BUND =
        GeoRoute(
            name = "Shanghai",
            timeZoneId = "Asia/Shanghai",
            offsetSeconds = 8 * SECONDS_PER_HOUR,
            datum = GpsDatum.Gcj02,
            vertices =
                listOf(
                    GeoPoint(31.2420, 121.4909),
                    GeoPoint(31.2382, 121.4903),
                    GeoPoint(31.2337, 121.4877),
                    GeoPoint(31.2304, 121.4762),
                    GeoPoint(31.2290, 121.4700),
                    GeoPoint(31.2261, 121.4658),
                ),
        )

    /** The home city — where the ordinary, non-trip days are shot. */
    val HOME_VANCOUVER =
        GeoRoute(
            name = "Vancouver",
            timeZoneId = "America/Vancouver",
            offsetSeconds = -7 * SECONDS_PER_HOUR,
            datum = GpsDatum.Wgs84,
            vertices =
                listOf(
                    GeoPoint(49.2827, -123.1207),
                    GeoPoint(49.2888, -123.1360),
                    GeoPoint(49.3017, -123.1440),
                    GeoPoint(49.2765, -123.1300),
                    GeoPoint(49.2634, -123.1387),
                    GeoPoint(49.2500, -123.1000),
                ),
        )

    /** Every route a trip can be planned onto, in a fixed order so the draw is reproducible. */
    val TRIP_ROUTES: List<GeoRoute> =
        listOf(ICELAND_SOUTH, KYOTO_WALK, MANHATTAN, BERNESE_OBERLAND, SHANGHAI_BUND)

    private const val SECONDS_PER_HOUR = 3600
}
