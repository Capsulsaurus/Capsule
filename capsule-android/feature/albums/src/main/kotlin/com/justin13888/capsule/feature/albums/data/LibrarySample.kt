package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.Asset

/**
 * A bounded, newest-first window of the library, read once and reused.
 *
 * Three surfaces in this feature — the hub's media-type counts, the duplicate finder and the
 * places map — are whole-library questions, and the timeline contract deliberately refuses to
 * hand out the whole library: a listing of a million assets cannot be emitted as a list at
 * all. So each of them works over a sample, and the sample is a shared object rather than
 * three private loops so they all agree on what "the library" meant and on how big the bite
 * was.
 *
 * The sample comes from the timeline window, so it excludes hidden, trashed and
 * stack-suppressed assets — the same exclusions every default view applies. That is the
 * correct scope for all three callers: none of them may show what the gated views hold.
 */
class LibrarySample(
    private val timeline: TimelineRepository,
    private val limit: Int = DEFAULT_LIMIT,
    private val pageSize: Int = DEFAULT_PAGE_SIZE,
) {
    /** Reads up to [limit] assets, newest first. */
    suspend fun assets(): SampledAssets {
        val out = ArrayList<Asset>(minOf(limit, pageSize))
        while (out.size < limit) {
            val window = timeline.timelineWindow(out.size, minOf(pageSize, limit - out.size))
            if (window.isEmpty()) break
            out += window
        }
        return SampledAssets(assets = out, isComplete = out.size < limit)
    }

    companion object {
        /** How many assets any one of these questions will look at before it stops. */
        const val DEFAULT_LIMIT = 4_096

        /** How much of the timeline is read per call. */
        const val DEFAULT_PAGE_SIZE = 512
    }
}

/**
 * What a sample found, and whether it saw everything.
 *
 * The flag travels with the assets because a count the user cannot trust is worse than no
 * count: "312 duplicates" and "312 duplicates among the 4,096 most recent" lead to different
 * decisions.
 */
data class SampledAssets(
    /** The assets read, newest first. */
    val assets: List<Asset>,
    /** Whether the sample reached the end of the library. */
    val isComplete: Boolean,
)
