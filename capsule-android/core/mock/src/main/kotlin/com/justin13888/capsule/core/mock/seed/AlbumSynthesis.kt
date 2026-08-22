package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.AlbumMember
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.DayBucket
import com.justin13888.capsule.core.model.asWire
import java.time.Duration
import java.time.LocalDate
import java.time.ZoneOffset

/**
 * The albums the library's history implies.
 *
 * Trip albums are derived from the trips the day planner already produced rather than being
 * drawn independently, because that is how they come to exist for real: a person goes
 * somewhere for a week and afterwards there is an album called that place and that year. An
 * album list drawn separately from the shooting history would have albums with no photos in
 * them and photo runs belonging to no album.
 */
internal fun LibraryGenerator.synthesizeAlbums(trips: List<Trip>) {
    val prng = accountPrng.derive("albums")
    val libraryStart = start.atStartOfDay().toInstant(ZoneOffset.UTC)
    // Nameless by design: a client surfaces the default album as the library's primary view,
    // never as a row called "Default", so the name is genuinely empty rather than a
    // placeholder string a locale would have to translate.
    defaultAlbum =
        Album(
            id = AlbumId(DeterministicIds.uuidV7(libraryStart.toEpochMilli(), prng)),
            name = "",
            createdAt = libraryStart,
            modifiedAt = spec.now,
            kind = AlbumKind.Container.asWire(),
            isDefault = true,
            viewerCapability = AlbumCapability.Admin.asWire(),
        )
    albums += defaultAlbum
    trips.forEach { trip ->
        val createdAt = trip.start.atStartOfDay().toInstant(ZoneOffset.UTC)
        val album =
            Album(
                id = AlbumId(DeterministicIds.uuidV7(createdAt.toEpochMilli(), prng)),
                name = trip.albumName,
                createdAt = createdAt,
                modifiedAt = createdAt.plus(Duration.ofDays(trip.dayCount.toLong())),
                kind = AlbumKind.Container.asWire(),
                viewerCapability = AlbumCapability.Admin.asWire(),
            )
        albums += album
        tripAlbums[trip.index] = album
    }
    albums += sharedContainer(prng, libraryStart)
    albums += studioContainer(prng, libraryStart)
    albums += favouritesView(prng, libraryStart)
}

/** The album shared with the rest of the household, so membership is exercised somewhere. */
private fun LibraryGenerator.sharedContainer(
    prng: Prng,
    libraryStart: java.time.Instant,
): Album =
    Album(
        id = AlbumId(DeterministicIds.uuidV7(libraryStart.toEpochMilli(), prng)),
        name = "Family",
        createdAt = libraryStart.plus(Duration.ofDays(FAMILY_ALBUM_OFFSET_DAYS)),
        modifiedAt = spec.now,
        kind = AlbumKind.Container.asWire(),
        viewerCapability = AlbumCapability.Admin.asWire(),
        members =
            listOf(
                AlbumMember(
                    userId = owner,
                    capability = AlbumCapability.Admin.asWire(),
                    joinedAt = libraryStart.plus(Duration.ofDays(FAMILY_ALBUM_OFFSET_DAYS)),
                ),
                AlbumMember(
                    userId =
                        com.justin13888.capsule.core.model
                            .UserId(DeterministicIds.uuidV7(0, prng)),
                    capability = AlbumCapability.Write.asWire(),
                    joinedAt = libraryStart.plus(Duration.ofDays(FAMILY_ALBUM_MEMBER_JOINED_DAYS)),
                ),
                AlbumMember(
                    userId =
                        com.justin13888.capsule.core.model
                            .UserId(DeterministicIds.uuidV7(0, prng)),
                    capability = AlbumCapability.Read.asWire(),
                    joinedAt = libraryStart.plus(Duration.ofDays(FAMILY_ALBUM_READER_JOINED_DAYS)),
                ),
            ),
    )

/** Where the paid work lands. */
private fun LibraryGenerator.studioContainer(
    prng: Prng,
    libraryStart: java.time.Instant,
): Album =
    Album(
        id = AlbumId(DeterministicIds.uuidV7(libraryStart.toEpochMilli(), prng)),
        name = "Studio — Client Work",
        createdAt = libraryStart.plus(Duration.ofDays(STUDIO_ALBUM_OFFSET_DAYS)),
        modifiedAt = spec.now,
        kind = AlbumKind.Container.asWire(),
        viewerCapability = AlbumCapability.Admin.asWire(),
    )

/**
 * A derived presentation, holding no key and owning no assets.
 *
 * Present so the difference between the two things the UI calls an "album" is reachable: a
 * view can never be an import destination, and a client that treated it as one would try to
 * file photos into something with no keys to encrypt them under.
 */
private fun LibraryGenerator.favouritesView(
    prng: Prng,
    libraryStart: java.time.Instant,
): Album =
    Album(
        id = AlbumId(DeterministicIds.uuidV7(libraryStart.toEpochMilli(), prng)),
        name = "Picks",
        createdAt = libraryStart.plus(Duration.ofDays(VIEW_ALBUM_OFFSET_DAYS)),
        modifiedAt = spec.now,
        kind = AlbumKind.View.asWire(),
    )

/**
 * Rewrites each album's asset count and cover from the assets that actually landed in it.
 *
 * Counted at the end rather than incremented as assets are added, because the state-matrix
 * pass moves assets into trash and hides others after the fact; a count maintained during
 * generation would be the number of assets ever filed there, not the number a user sees.
 */
internal fun LibraryGenerator.finalizeAlbums() {
    val byAlbum = assets.groupBy(Asset::albumId)
    for (index in albums.indices) {
        val album = albums[index]
        val members = byAlbum[album.id].orEmpty()
        albums[index] =
            album.copy(
                assetCount = members.count(Asset::isInDefaultViews),
                coverAssetId = members.firstOrNull(Asset::isInDefaultViews)?.id,
            )
    }
}

/**
 * The day buckets for a list of assets already in timeline order.
 *
 * Lives here rather than in the timeline repository because both the library-wide index and
 * every album's own index need it, and two implementations of "which day is this" would
 * eventually disagree about the boundary.
 */
internal fun dayBucketsOf(ordered: List<Asset>): List<DayBucket> {
    val buckets = mutableListOf<DayBucket>()
    var currentDay: LocalDate? = null
    var runStart = 0
    ordered.forEachIndexed { index, asset ->
        val day = asset.captureTimestamp.atZone(ZoneOffset.UTC).toLocalDate()
        if (day != currentDay) {
            currentDay?.let { buckets += DayBucket(it, runStart, index - runStart) }
            currentDay = day
            runStart = index
        }
    }
    currentDay?.let { buckets += DayBucket(it, runStart, ordered.size - runStart) }
    return buckets
}

private const val FAMILY_ALBUM_OFFSET_DAYS = 30L
private const val FAMILY_ALBUM_MEMBER_JOINED_DAYS = 45L
private const val FAMILY_ALBUM_READER_JOINED_DAYS = 400L
private const val STUDIO_ALBUM_OFFSET_DAYS = 60L
private const val VIEW_ALBUM_OFFSET_DAYS = 120L
