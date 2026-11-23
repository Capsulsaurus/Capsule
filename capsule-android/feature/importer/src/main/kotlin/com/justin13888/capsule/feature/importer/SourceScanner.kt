package com.justin13888.capsule.feature.importer

/**
 * Turns one source into the candidate files the planner reasons about.
 *
 * A port rather than a call into `MediaStore` here, because the planner's determinism
 * contract starts at this boundary: a given source must yield the same candidate list on
 * every run, or the plan the user confirms is not the plan that executes. Keeping the walk
 * behind an interface is also what lets the plan screen be tested without a media store.
 */
fun interface SourceScanner {
    /** Every candidate file under [source], in a stable order. */
    suspend fun scan(source: ImportSource): List<String>
}

/**
 * A stand-in scanner that derives a stable candidate list from the source's own locator.
 *
 * The real Android walk is a `MediaStore` query filtered by `RELATIVE_PATH`, which this build
 * has no permission plumbing for yet. Deriving the list from the locator keeps the two
 * properties that matter downstream — the same source yields the same files, and different
 * sources never collide — so the planner, the plan screen and the run screen are exercised
 * against exactly the shape the real scanner will hand them.
 */
class DeterministicSourceScanner(
    /** The largest number of candidates any one source produces. */
    private val maxFiles: Int = DEFAULT_MAX_FILES,
) : SourceScanner {
    override suspend fun scan(source: ImportSource): List<String> {
        val spread = source.locator.fold(0) { acc, char -> acc * PRIME + char.code }
        val count = MIN_FILES + Math.floorMod(spread, maxFiles - MIN_FILES + 1)
        return (1..count).map { index -> "${source.locator}/IMG_%04d.JPG".format(index) }
    }

    private companion object {
        /** Enough files that the plan's counts and the run's list are worth reading. */
        const val DEFAULT_MAX_FILES = 42

        /** Every real source has at least this many, so no source scans to nothing. */
        const val MIN_FILES = 6

        /** An odd multiplier, so two locators differing in one character do not collide. */
        const val PRIME = 31
    }
}
