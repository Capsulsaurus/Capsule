package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.Predicate

/**
 * How many assets a predicate would match, answered before it is saved.
 *
 * A port of its own rather than a call on the search contract, because the question is
 * genuinely different: `SearchRepository.evaluate` answers for a definition the library
 * already holds, and the builder needs an answer for one the user is still typing. Saving a
 * scratch definition to ask would put a half-written rule into the synced settings document
 * and into every other device's smart-album list.
 */
interface SmartAlbumPreview {
    /**
     * The number of assets [predicate] currently matches.
     *
     * Computed over a bounded, newest-first sample of the library rather than over all of
     * it: the count is recomputed on every edit, and a scan of a million assets per keystroke
     * would make the builder unusable. [SampledMatchCount.isComplete] says whether the sample
     * covered everything, so the screen can be honest about an approximate figure instead of
     * quietly rounding.
     */
    suspend fun count(predicate: Predicate): SampledMatchCount
}

/**
 * A match count and whether it is the whole answer.
 *
 * The flag travels with the number because a count the user cannot trust is worse than no
 * count: "1,204 matches" and "at least 1,204 matches, of the 4,096 most recent" lead to
 * different decisions about whether the rule is right.
 */
data class SampledMatchCount(
    /** How many of the sampled assets matched. */
    val matches: Int,
    /** How many assets were looked at. */
    val sampled: Int,
    /** Whether the sample was the entire library. */
    val isComplete: Boolean,
)

/**
 * Counts matches locally, over a bounded window of the timeline.
 *
 * Reads nothing from the network, which is not an optimisation but a requirement: a gallery
 * read path performs no network I/O at all, and the builder is a gallery read path wearing a
 * different hat.
 *
 * The sample is drawn from the timeline, so it excludes hidden, trashed and stack-suppressed
 * assets — the same exclusions every default view applies. A rule written over `hidden` will
 * therefore preview as zero even though it evaluates non-empty once saved; the alternative,
 * paging a separate hidden window into the preview, would put the contents of a locally
 * gated view in front of someone who has not passed the gate.
 */
class LocalSmartAlbumPreview(
    private val sample: LibrarySample,
) : SmartAlbumPreview {
    override suspend fun count(predicate: Predicate): SampledMatchCount {
        val sampled = sample.assets()
        return SampledMatchCount(
            matches = sampled.assets.count { PredicateMatcher.matches(predicate, it) },
            sampled = sampled.assets.size,
            isComplete = sampled.isComplete,
        )
    }
}
