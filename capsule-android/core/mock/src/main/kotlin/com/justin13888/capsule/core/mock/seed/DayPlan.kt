package com.justin13888.capsule.core.mock.seed

import java.time.LocalDate

/**
 * What kind of day this was, which is what decides how many frames it produced and what they
 * look like.
 *
 * A library generated at a flat rate per day is immediately recognisable as fake, and worse,
 * it never exercises the layout engine the way a real one does: no long empty scroll, no day
 * whose section is four screens tall, no run of near-identical frames a burst has to collapse.
 * The kinds below exist so every one of those shapes is somewhere in the fixture.
 */
enum class DayKind(
    /** Relative share of the library's assets that a day of this kind takes. */
    val weight: Double,
    /** How much a single day's weight is allowed to wander, as a fraction of [weight]. */
    val jitter: Double,
) {
    /** Nothing was shot. The empty stretches a scroll bar has to cross. */
    Empty(weight = 0.0, jitter = 0.0),

    /** A weekday with a handful of phone frames. */
    Ordinary(weight = 1.0, jitter = 0.6),

    /** A weekend — an outing, and several times an ordinary day's frames. */
    Weekend(weight = 3.2, jitter = 0.5),

    /** A day of a multi-day trip. The heaviest days in the library. */
    Trip(weight = 7.5, jitter = 0.4),

    /** A paid shoot: one location, one body, a lot of frames, heavily culled afterwards. */
    StudioShoot(weight = 9.0, jitter = 0.3),

    /** A run of screenshots — a receipt, a recipe, a bug report. No optics, no GPS. */
    ScreenshotRun(weight = 1.4, jitter = 0.5),
}

/** One trip: where it went, when it started, and how long it ran. */
data class Trip(
    /** Its position in the library's trip list, oldest first. */
    val index: Int,
    /** Where it went. */
    val route: GeoRoute,
    /** The first day. */
    val start: LocalDate,
    /** How many days it covered. */
    val dayCount: Int,
) {
    /** One past the last day. */
    val endExclusive: LocalDate get() = start.plusDays(dayCount.toLong())

    /** The album name a trip produces — a place and a year, the way people actually name them. */
    val albumName: String get() = "${route.name} ${start.year}"

    /** Whether [date] falls inside the trip. */
    fun covers(date: LocalDate): Boolean = !date.isBefore(start) && date.isBefore(endExclusive)
}

/** One day of the plan, with the count the allocator gave it. */
data class PlannedDay(
    /** The local calendar day. */
    val date: LocalDate,
    /** What kind of day it was. */
    val kind: DayKind,
    /** Where it was shot. */
    val route: GeoRoute,
    /** The trip it belongs to, when it belongs to one. */
    val trip: Trip?,
    /** How many assets land on this day. */
    val assetCount: Int,
)

/**
 * Turns a seed and a date span into a day-by-day shooting schedule.
 *
 * Planning is separated from asset generation because the total has to be decided before any
 * single day can be filled: the caller asks for roughly forty thousand assets, and the only
 * way to honour that exactly while still letting individual days vary wildly is to weight
 * every day first and then allocate the budget across the weights.
 */
object DayPlanner {
    /**
     * Plans [days] of shooting ending the day before [endExclusive], totalling exactly
     * [targetAssetCount] assets.
     *
     * The allocation is a cumulative-rounding walk rather than a per-day rounding: rounding
     * each day independently loses or gains a few hundred assets over five years, and a
     * fixture whose size drifts with its seed cannot be asserted on.
     */
    fun plan(
        start: LocalDate,
        endExclusive: LocalDate,
        targetAssetCount: Int,
        prng: Prng,
    ): List<PlannedDay> {
        val trips = planTrips(start, endExclusive, prng.derive("trips"))
        val kindPrng = prng.derive("day-kinds")
        val dates = generateSequence(start) { it.plusDays(1) }.takeWhile { it.isBefore(endExclusive) }.toList()
        val kinds = dates.map { date -> classify(date, trips, kindPrng) }
        val weights = kinds.map { (kind, _) -> weightOf(kind, kindPrng) }
        val counts = allocate(weights, targetAssetCount)
        return dates.mapIndexed { index, date ->
            val (kind, trip) = kinds[index]
            PlannedDay(
                date = date,
                kind = kind,
                route = trip?.route ?: GeoRoutes.HOME_VANCOUVER,
                trip = trip,
                assetCount = counts[index],
            )
        }
    }

    /** Every trip in a plan, oldest first — the source of the library's trip albums. */
    fun planTrips(
        start: LocalDate,
        endExclusive: LocalDate,
        prng: Prng,
    ): List<Trip> {
        val trips = mutableListOf<Trip>()
        var year = start.year
        var index = 0
        while (LocalDate.of(year, 1, 1).isBefore(endExclusive)) {
            repeat(prng.nextInt(TRIPS_PER_YEAR_MIN, TRIPS_PER_YEAR_MAX)) { withinYear ->
                val month = FIRST_TRIP_MONTH + withinYear * TRIP_MONTH_STRIDE
                if (month <= LAST_TRIP_MONTH) {
                    val day = prng.nextInt(1, TRIP_START_DAY_MAX)
                    val tripStart = LocalDate.of(year, month, day)
                    if (!tripStart.isBefore(start) && tripStart.isBefore(endExclusive)) {
                        val route = GeoRoutes.TRIP_ROUTES[index % GeoRoutes.TRIP_ROUTES.size]
                        trips +=
                            Trip(
                                index = index,
                                route = route,
                                start = tripStart,
                                dayCount = prng.nextInt(TRIP_DAYS_MIN, TRIP_DAYS_MAX),
                            )
                        index++
                    }
                }
            }
            year++
        }
        return trips
    }

    /** Which kind of day [date] is, and which trip it belongs to when it belongs to one. */
    private fun classify(
        date: LocalDate,
        trips: List<Trip>,
        prng: Prng,
    ): Pair<DayKind, Trip?> {
        val trip = trips.firstOrNull { it.covers(date) }
        if (trip != null) return DayKind.Trip to trip
        val roll = prng.nextDouble()
        val isWeekend = date.dayOfWeek.value >= SATURDAY
        return when {
            roll < EMPTY_SHARE -> DayKind.Empty to null
            roll < EMPTY_SHARE + SCREENSHOT_SHARE -> DayKind.ScreenshotRun to null
            roll < EMPTY_SHARE + SCREENSHOT_SHARE + STUDIO_SHARE && !isWeekend -> DayKind.StudioShoot to null
            isWeekend -> DayKind.Weekend to null
            else -> DayKind.Ordinary to null
        }
    }

    /** A day's weight, jittered inside its kind's band so no two days of a kind match. */
    private fun weightOf(
        kind: DayKind,
        prng: Prng,
    ): Double {
        if (kind.weight <= 0.0) return 0.0
        val swing = prng.nextDouble(-kind.jitter, kind.jitter)
        return (kind.weight * (1.0 + swing)).coerceAtLeast(MIN_WEIGHT)
    }

    /**
     * Splits [total] across [weights] so the parts sum to exactly [total].
     *
     * Each day gets the difference between two cumulative roundings, which is the standard
     * largest-remainder trick: no day is systematically rounded down, and the sum is exact by
     * construction rather than by a corrective pass at the end.
     */
    private fun allocate(
        weights: List<Double>,
        total: Int,
    ): List<Int> {
        val sum = weights.sum()
        if (sum <= 0.0) return List(weights.size) { 0 }
        val counts = IntArray(weights.size)
        var running = 0.0
        var placed = 0
        weights.forEachIndexed { index, weight ->
            running += weight
            val throughHere = ((running / sum) * total).toInt()
            counts[index] = throughHere - placed
            placed = throughHere
        }
        return counts.toList()
    }

    private const val SATURDAY = 6
    private const val EMPTY_SHARE = 0.14
    private const val SCREENSHOT_SHARE = 0.09
    private const val STUDIO_SHARE = 0.07
    private const val MIN_WEIGHT = 0.05
    private const val TRIPS_PER_YEAR_MIN = 2
    private const val TRIPS_PER_YEAR_MAX = 3
    private const val FIRST_TRIP_MONTH = 3
    private const val TRIP_MONTH_STRIDE = 4
    private const val LAST_TRIP_MONTH = 11
    private const val TRIP_START_DAY_MAX = 24
    private const val TRIP_DAYS_MIN = 4
    private const val TRIP_DAYS_MAX = 11
}
