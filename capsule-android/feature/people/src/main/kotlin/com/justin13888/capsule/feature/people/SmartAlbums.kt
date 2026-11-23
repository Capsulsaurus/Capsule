package com.justin13888.capsule.feature.people

import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import java.time.Duration
import java.time.ZoneOffset

/**
 * The automatically built albums, one per row of the smart-albums feature doc.
 *
 * People are deliberately not one of these. The doc lists "one album per person", but a
 * person is the *user's* contribution — a name they typed and the clusters they confirmed —
 * so it belongs on the hub above, next to the naming and merging actions, rather than in a
 * list of things a model decided.
 */
enum class SmartAlbumKind {
    /** GPS and time clustered together: a run of located captures with a gap either side. */
    Trips,

    /** Pet detection. */
    Pets,

    /** Food recognition. */
    Food,

    /** Scene and animal classification. */
    Nature,

    /** Aesthetic quality within the most recent year of shooting. */
    BestOfYear,
}

/** One built album: the kind, and the assets it currently matches. */
data class SmartAlbumMembership(
    /** Which album. */
    val kind: SmartAlbumKind,
    /** Its members, newest capture first. */
    val assets: List<Asset>,
)

/**
 * Builds the AI smart albums over a window of the library.
 *
 * Three properties are load-bearing and all three are checkable here, with no repository in
 * the way.
 *
 * **Membership is computed, never stored.** Nothing in this file writes: an album is a
 * function of the assets and their AI-namespaced tags, so editing an asset moves and
 * re-encrypts nothing, and a stale membership cannot exist to go wrong.
 *
 * **Only the AI namespace feeds them.** A user's own `food` tag does not put a photo in
 * Food. The two namespaces are separate fields precisely so a model can never reach into
 * user intent, and reading across them here would quietly undo that separation in the one
 * place the user would never think to look.
 *
 * **Superseded outputs do not count.** `AssetTags.freshAi` drops entries whose model version
 * has been replaced, so an album shrinks honestly after a model swap and grows back as
 * regeneration lands — rather than mixing values from two incomparable vector spaces.
 */
object SmartAlbums {
    /** How long a gap has to be before it separates one trip from the next. */
    val TRIP_GAP: Duration = Duration.ofDays(2)

    /** How many located captures a run needs before it counts as a trip rather than an errand. */
    const val MIN_TRIP_ASSETS: Int = 4

    /** The stars a photo needs to be one of the year's best. */
    const val BEST_OF_YEAR_STARS: Int = 4

    private val PET_WORDS = setOf("dog", "cat", "pet", "puppy", "kitten")

    private val FOOD_WORDS = setOf("food", "meal", "restaurant", "coffee", "dinner")

    private val NATURE_WORDS = setOf("mountain", "beach", "snow", "water", "forest", "wildlife", "tree")

    /** Every album, in the order the hub lists them. Empty ones are included so the set is stable. */
    fun build(
        assets: List<Asset>,
        tags: Map<AssetId, AssetTags>,
    ): List<SmartAlbumMembership> =
        listOf(
            SmartAlbumMembership(SmartAlbumKind.Trips, trips(assets)),
            SmartAlbumMembership(SmartAlbumKind.Pets, tagged(assets, tags, PET_WORDS)),
            SmartAlbumMembership(SmartAlbumKind.Food, tagged(assets, tags, FOOD_WORDS)),
            SmartAlbumMembership(SmartAlbumKind.Nature, tagged(assets, tags, NATURE_WORDS)),
            SmartAlbumMembership(SmartAlbumKind.BestOfYear, bestOfYear(assets)),
        )

    /**
     * Located captures that arrived in a burst, with quiet either side.
     *
     * The clustering is over time alone even though the doc calls it GPS *and* temporal: the
     * GPS half is the presence of a fix, which is what distinguishes "I went somewhere" from
     * "I photographed the kitchen", and adding a distance threshold on top would need a datum
     * conversion the design forbids doing at rest.
     */
    fun trips(assets: List<Asset>): List<Asset> {
        val located = assets.filter { it.gps != null }.sortedBy(Asset::captureTimestamp)
        if (located.isEmpty()) return emptyList()
        val runs = mutableListOf<MutableList<Asset>>(mutableListOf(located.first()))
        located.zipWithNext { previous, next ->
            val gap = Duration.between(previous.captureTimestamp, next.captureTimestamp)
            if (gap > TRIP_GAP) runs += mutableListOf(next) else runs.last() += next
        }
        return runs
            .filter { it.size >= MIN_TRIP_ASSETS }
            .flatten()
            .sortedByDescending(Asset::captureTimestamp)
    }

    /**
     * Assets whose current model suggestions intersect [vocabulary].
     *
     * The vocabulary is a set of tag words rather than a model call because the tags *are*
     * the model's output — the inference has already happened, and re-deriving a category
     * from pixels at display time would be a second, disagreeing answer.
     */
    fun tagged(
        assets: List<Asset>,
        tags: Map<AssetId, AssetTags>,
        vocabulary: Set<String>,
    ): List<Asset> =
        assets
            .filter { asset ->
                tags[asset.id]
                    ?.freshAi
                    .orEmpty()
                    .any { it.tag.lowercase() in vocabulary }
            }.sortedByDescending(Asset::captureTimestamp)

    /**
     * The best of the most recent year present.
     *
     * "Most recent year present" rather than "this year" on purpose: a library nobody has
     * added to since March would otherwise show an empty album for nine months, which reads
     * as a broken feature rather than as an accurate one.
     */
    fun bestOfYear(assets: List<Asset>): List<Asset> {
        val latestYear = assets.maxOfOrNull { yearOf(it) } ?: return emptyList()
        return assets
            .filter { yearOf(it) == latestYear && it.rating >= BEST_OF_YEAR_STARS }
            .sortedWith(compareByDescending(Asset::rating).thenByDescending(Asset::captureTimestamp))
    }

    /** The tag words behind one album, so a test can state the vocabulary it is asserting. */
    fun vocabularyOf(kind: SmartAlbumKind): Set<String> =
        when (kind) {
            SmartAlbumKind.Pets -> PET_WORDS
            SmartAlbumKind.Food -> FOOD_WORDS
            SmartAlbumKind.Nature -> NATURE_WORDS
            SmartAlbumKind.Trips, SmartAlbumKind.BestOfYear -> emptySet()
        }

    private fun yearOf(asset: Asset): Int = (asset.captureUtc ?: asset.captureTimestamp).atZone(ZoneOffset.UTC).year
}

/** Every current model suggestion on an asset, lowercased for comparison. */
internal fun AssetTags.freshAiWords(): List<String> = freshAi.map(AiTag::tag)
