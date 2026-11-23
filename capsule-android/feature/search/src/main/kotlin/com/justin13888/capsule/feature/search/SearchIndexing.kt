package com.justin13888.capsule.feature.search

import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.MediaKind
import com.justin13888.capsule.core.model.ModelOutputStatus
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.UserTag
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.collections.immutable.toImmutableList
import java.time.Duration
import java.time.ZoneOffset

/** Drawn behind a thumbnail that has no placeholder of its own. */
private const val NEUTRAL_PLACEHOLDER = 0x2A2A2A

/** The most stars an asset can carry, from the catalog's `rating` column. */
private const val MAX_STARS = 5

private const val SECONDS_PER_MINUTE = 60L

private const val MINUTES_PER_HOUR = 60L

/**
 * Turns catalog records into the facet space the screen filters over.
 *
 * A separate object from the view model, and pure, because everything interesting about
 * search on this screen happens here: which points an asset sits on, which model outputs are
 * being withheld, and where the grid's year boundaries fall. All three are checkable without
 * a repository, a coroutine or a composition.
 */
object SearchIndexing {
    /**
     * Builds the searchable projection of one window of hits.
     *
     * @param assets the hits, already in the order the grid will show them.
     * @param tags each hit's two tag namespaces, keyed by asset.
     * @param peopleByAsset which named people appear in each asset; see [peopleIndex].
     * @param albums every album, so an asset's container can be named rather than numbered.
     */
    fun candidates(
        assets: List<Asset>,
        tags: Map<AssetId, AssetTags>,
        peopleByAsset: Map<AssetId, List<FacetPoint>>,
        albums: List<Album>,
    ): List<SearchCandidate> {
        val albumsById = albums.associateBy(Album::id)
        return assets.map { asset ->
            SearchCandidate(
                item = itemOf(asset),
                points =
                    pointsOf(
                        asset = asset,
                        tags = tags[asset.id] ?: AssetTags(),
                        people = peopleByAsset[asset.id].orEmpty(),
                        album = asset.albumId?.let(albumsById::get),
                    ).toImmutableList(),
            )
        }
    }

    /** The grid's view of one asset. */
    fun itemOf(asset: Asset): SearchResultItem =
        SearchResultItem(
            assetId = asset.id.value,
            // A record the extractor could not probe still has to occupy a cell, and a
            // square is the least wrong guess: it biases neither portrait nor landscape.
            aspectRatio = asset.dimensions?.aspectRatio?.takeIf { it > 0f } ?: 1f,
            year = yearOf(asset),
            placeholder =
                asset.lqip?.dominantColor?.let { (it.red shl 16) or (it.green shl 8) or it.blue }
                    ?: NEUTRAL_PLACEHOLDER,
            videoDuration = asset.duration?.let(::formatDuration),
        )

    /**
     * Every facet point one asset sits on.
     *
     * Unknown wire tokens become points of their own rather than being dropped. A value a
     * newer peer wrote is still a fact about the library, and a filter list that quietly
     * omitted it would make those assets unreachable from this screen.
     */
    @Suppress("LongMethod")
    fun pointsOf(
        asset: Asset,
        tags: AssetTags,
        people: List<FacetPoint>,
        album: Album?,
    ): List<FacetPoint> {
        val points = mutableListOf<FacetPoint>()
        val year = yearOf(asset)
        points += FacetPoint(FacetRef(FacetKind.Date, year.toString()), FacetLabel.Data(year.toString()))
        points += mediaPoint(asset)
        points += ratingPoint(asset.rating)
        points += cullPoint(asset.cull)
        // Both namespaces feed one facet, de-duplicated by text: the user asking for "beach"
        // does not care which side of the AI/user split the word came from. Stale AI tags are
        // absent by construction — `freshAi` drops them — which is the same exclusion the
        // stale-model notice explains.
        (tags.user.map(UserTag::tag) + tags.freshAi.map(AiTag::tag))
            .distinct()
            .forEach { tag -> points += FacetPoint(FacetRef(FacetKind.Tag, tag), FacetLabel.Data(tag)) }
        points += people
        points += placePoint(asset)
        points += cameraPoint(asset)
        points += albumPoint(asset.albumId, album)
        return points
    }

    /**
     * Which named people appear in which assets.
     *
     * Only clusters on the canonical model slot contribute. A superseded cluster is kept and
     * queued for regeneration rather than deleted, but it must not answer a query — the
     * vector spaces differ across model versions, so a match through one would be a
     * comparison the design forbids. [staleNotice] is what tells the user that happened.
     */
    fun peopleIndex(
        people: List<Person>,
        clusters: List<FaceCluster>,
    ): Map<AssetId, List<FacetPoint>> {
        val personByCluster = people.flatMap { person -> person.clusterIds.map { it to person } }.toMap()
        val index = LinkedHashMap<AssetId, MutableList<FacetPoint>>()
        clusters
            .filter { it.status.knownOrNull == ModelOutputStatus.Current }
            .mapNotNull { cluster -> personByCluster[cluster.id]?.let { cluster.assetIds to personPoint(it) } }
            .forEach { (assetIds, point) -> assetIds.forEach { attach(index, it, point) } }
        return index
    }

    /**
     * What the current query is not allowed to see, or `null` when every slot is canonical.
     *
     * Counts both halves of the exclusion because a user sees one symptom — fewer results —
     * for two causes: a face cluster whose embeddings came from a superseded model, and an
     * AI tag from a superseded tagger.
     */
    fun staleNotice(
        clusters: List<FaceCluster>,
        tags: Collection<AssetTags>,
    ): StaleModelNotice? {
        val staleClusters = clusters.filter { it.status.knownOrNull != ModelOutputStatus.Current }
        val staleTags = tags.flatMap(AssetTags::ai).filter(AiTag::isStale)
        val excluded = staleClusters.size + staleTags.size
        if (excluded == 0) return null
        val slots = (staleClusters.map(FaceCluster::slot) + staleTags.map(AiTag::slot)).map(::spell).distinct()
        return StaleModelNotice(excludedCount = excluded, slots = slots.sorted().toImmutableList())
    }

    /**
     * Where each year begins in an already-ordered result list.
     *
     * The grid's sections are years rather than days: a search answer spans the whole
     * library, and day headers over a hundred scattered hits would be one header per row.
     */
    fun sectionStarts(items: List<SearchResultItem>): IntArray {
        if (items.isEmpty()) return IntArray(0)
        val starts = mutableListOf(0)
        for (index in 1 until items.size) {
            if (items[index].year != items[index - 1].year) starts += index
        }
        return starts.toIntArray()
    }
}

/** `model-id version`, the spelling the stale notice lists slots by. */
private fun spell(slot: ModelSlot): String = "${slot.modelId.value} ${slot.modelVersion.value}"

/**
 * A running time as `m:ss`, or `h:mm:ss` once it passes an hour.
 *
 * Formatted here rather than in the composition because the gallery's cell chrome takes
 * an already-formatted string: it draws the marker and owns the announced label, and has
 * no business knowing what a `Duration` is.
 */
private fun formatDuration(duration: Duration): String {
    val total = duration.seconds.coerceAtLeast(0)
    val seconds = total % SECONDS_PER_MINUTE
    val minutes = (total / SECONDS_PER_MINUTE) % MINUTES_PER_HOUR
    val hours = total / (SECONDS_PER_MINUTE * MINUTES_PER_HOUR)
    return if (hours > 0) {
        "%d:%02d:%02d".format(hours, minutes, seconds)
    } else {
        "%d:%02d".format(minutes, seconds)
    }
}

private fun yearOf(asset: Asset): Int = (asset.captureUtc ?: asset.captureTimestamp).atZone(ZoneOffset.UTC).year

private fun mediaPoint(asset: Asset): FacetPoint {
    val kind = asset.contentType?.knownOrNull?.mediaKind
    return when {
        kind != null -> FacetPoint(FacetRef(FacetKind.MediaKind, kind.wire), termFor(kind))
        asset.assetType.knownOrNull == AssetType.Video ->
            FacetPoint(FacetRef(FacetKind.MediaKind, AssetType.Video.wire), FacetLabel.Term(FacetTerm.Video))

        asset.assetType.knownOrNull == AssetType.Photo ->
            FacetPoint(FacetRef(FacetKind.MediaKind, AssetType.Photo.wire), FacetLabel.Term(FacetTerm.Image))

        else ->
            FacetPoint(
                FacetRef(FacetKind.MediaKind, asset.assetType.raw),
                FacetLabel.Data(asset.assetType.raw),
            )
    }
}

private fun termFor(kind: MediaKind): FacetLabel =
    when (kind) {
        MediaKind.Image -> FacetLabel.Term(FacetTerm.Image)
        MediaKind.Video -> FacetLabel.Term(FacetTerm.Video)
    }

private fun ratingPoint(rating: Int): FacetPoint {
    val stars = rating.coerceIn(0, MAX_STARS)
    val label = if (stars == 0) FacetLabel.Term(FacetTerm.Unrated) else FacetLabel.Stars(stars)
    return FacetPoint(FacetRef(FacetKind.Rating, stars.toString()), label)
}

private fun cullPoint(cull: Wire<CullFlag>): FacetPoint {
    val label =
        when (cull.knownOrNull) {
            CullFlag.Pick -> FacetLabel.Term(FacetTerm.Picked)
            CullFlag.Neutral -> FacetLabel.Term(FacetTerm.Unreviewed)
            CullFlag.Reject -> FacetLabel.Term(FacetTerm.Rejected)
            null -> FacetLabel.Data(cull.raw)
        }
    return FacetPoint(FacetRef(FacetKind.Cull, cull.raw), label)
}

private fun placePoint(asset: Asset): FacetPoint =
    if (asset.gps == null) {
        FacetPoint(FacetRef(FacetKind.Place, "absent"), FacetLabel.Term(FacetTerm.Unlocated))
    } else {
        FacetPoint(FacetRef(FacetKind.Place, "present"), FacetLabel.Term(FacetTerm.Located))
    }

private fun cameraPoint(asset: Asset): FacetPoint {
    val name = listOfNotNull(asset.camera?.make, asset.camera?.model).joinToString(" ").trim()
    return if (name.isEmpty()) {
        FacetPoint(FacetRef(FacetKind.Camera, "none"), FacetLabel.Term(FacetTerm.NoCamera))
    } else {
        FacetPoint(FacetRef(FacetKind.Camera, name), FacetLabel.Data(name))
    }
}

private fun albumPoint(
    albumId: AlbumId?,
    album: Album?,
): FacetPoint =
    when {
        albumId == null -> FacetPoint(FacetRef(FacetKind.Album, "none"), FacetLabel.Term(FacetTerm.NoAlbum))
        album == null || album.name.isEmpty() ->
            FacetPoint(FacetRef(FacetKind.Album, albumId.value), FacetLabel.Term(FacetTerm.DefaultAlbum))

        else -> FacetPoint(FacetRef(FacetKind.Album, albumId.value), FacetLabel.Data(album.name))
    }

/** The facet point one named person contributes. */
private fun personPoint(person: Person): FacetPoint =
    FacetPoint(FacetRef(FacetKind.Person, person.id.value), FacetLabel.Data(person.displayName))

/**
 * Records that [point] applies to [assetId], once.
 *
 * Two clusters can be attached to the same person — that is exactly what a merge produces —
 * so an asset appearing in both would otherwise be counted twice in the person facet.
 */
private fun attach(
    index: MutableMap<AssetId, MutableList<FacetPoint>>,
    assetId: AssetId,
    point: FacetPoint,
) {
    val held = index.getOrPut(assetId) { mutableListOf() }
    if (held.none { it.ref == point.ref }) held += point
}
