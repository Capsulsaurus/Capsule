package com.justin13888.capsule.core.mock.seed

import java.time.Instant
import java.time.ZoneId

/**
 * Everything that decides what a generated library contains.
 *
 * A single value rather than a pile of parameters, because it is also the identity of the
 * fixture: two [SeededLibrary] runs agree byte for byte exactly when their specs are equal,
 * which is what lets a screenshot golden name the spec it was captured under.
 *
 * [now] is a parameter and not a clock read, for the same reason. A generator that called
 * `Instant.now()` would place the trash retention windows relative to whenever the test
 * happened to run, and "this asset is four days into a thirty-day window" would silently
 * become "this asset is purgeable" the following month.
 */
data class LibrarySpec(
    /** The root seed. Every draw in the generator descends from this one number. */
    val seed: Long = DEFAULT_SEED,
    /** The instant the library is generated "as of" — the anchor for every relative date. */
    val now: Instant = DEFAULT_NOW,
    /** How many years of shooting history to produce. */
    val years: Int = DEFAULT_YEARS,
    /** How many assets to produce, hit exactly rather than approximately. */
    val targetAssetCount: Int = DEFAULT_ASSET_COUNT,
    /** The zone the owner lives in, and therefore the zone ordinary days are shot in. */
    val homeZone: ZoneId = ZoneId.of("America/Vancouver"),
) {
    companion object {
        /** The seed the app and the screenshot goldens run on. */
        const val DEFAULT_SEED = 0x0C0FFEE_5EEDL

        /** Five years of history. */
        const val DEFAULT_YEARS = 5

        /** The professional-library size the design targets. */
        const val DEFAULT_ASSET_COUNT = 40_000

        /** The library's "today". Fixed, so relative dates in goldens never drift. */
        val DEFAULT_NOW: Instant = Instant.parse("2026-06-15T19:00:00Z")

        /** The full-size fixture — five years, forty thousand assets. */
        val DEFAULT = LibrarySpec()

        /**
         * A one-year, two-thousand-asset fixture.
         *
         * For tests that assert on structure rather than on scale. The full spec takes long
         * enough to build that using it for every case would make the suite slow enough to
         * stop being run, which is a worse outcome than a smaller fixture.
         */
        val SMALL = LibrarySpec(years = 1, targetAssetCount = 2_000)

        /** An empty library — the first-run state, before anything has been imported. */
        val EMPTY = LibrarySpec(years = 1, targetAssetCount = 0)
    }
}
