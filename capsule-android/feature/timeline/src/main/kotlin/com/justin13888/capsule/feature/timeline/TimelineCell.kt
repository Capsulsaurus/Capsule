package com.justin13888.capsule.feature.timeline

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.MediaKind
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.RgbColor
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.core.ui.gallery.GalleryCellMarkers
import com.justin13888.capsule.core.ui.gallery.GalleryCellSync
import java.time.Duration
import java.time.Instant

/**
 * The star rating that stands in for "favourite".
 *
 * The catalog has no favourite register of its own — an asset carries a `rating` and a
 * `cull` flag and nothing else — so a favourite is the top of the rating scale rather than a
 * fourth piece of state this feature invents. Inventing one would be a second source of
 * truth that no other client, and no sync, would ever agree with.
 */
const val FAVOURITE_RATING: Int = 5

/**
 * The four ways an asset can be listed but not fully renderable.
 *
 * Each is non-destructive and each is distinct, because the remedy differs: an original that
 * has not left its capturing device will arrive on its own, a `410 Gone` never will, a file
 * this device cannot decode is a codec problem rather than a storage one, and a quarantined
 * asset is waiting on a human. Collapsing them into one "broken" badge would tell the user
 * nothing they could act on.
 */
enum class TimelineDegradation {
    /** The original has not been uploaded from the device that captured it yet. */
    AwaitingOriginal,

    /** Every rung above the embedded placeholder is unreachable from here. */
    FullResolutionUnavailable,

    /** The media sandbox refused to decode these bytes after its retry budget. */
    UnreadableOnThisDevice,

    /** Inbound state was refused and parked for a human decision. */
    Quarantined,
}

/**
 * One thumbnail's worth of an asset — everything the grid draws, and nothing it does not.
 *
 * Projected from [Asset] rather than passing the record through, for two reasons. The grid
 * redraws on every scrolled pixel, and an `Asset` carries GPS, camera serials and provenance
 * that a tile has no business holding resident forty thousand times over. And the projection
 * is pure, so every "does a burst show its count", "is a quarantined asset marked" question
 * is answerable in a unit test with no renderer at all.
 *
 * @param assetId the identity the viewer, the selection actions and the cull writes use.
 * @param markers the chrome the shared gallery cell draws.
 * @param degradation why this tile is not the full thing, or `null` when it is.
 * @param placeholder the LQIP's dominant colour — the ladder's floor, always present.
 * @param capturedAt the sort key, used for the announced label.
 * @param isVideo whether the tile is a moving image, for the announced label.
 * @param cull the culling flag, or `null` when a newer peer wrote a token outside the set.
 * @param rating stars, `0..5`, deliberately independent of [cull].
 * @param stackId the stack this asset speaks for, so the grid can open it expanded.
 */
@Immutable
data class TimelineCell(
    val assetId: AssetId,
    val markers: GalleryCellMarkers,
    val degradation: TimelineDegradation?,
    val placeholder: Color,
    val capturedAt: Instant,
    val isVideo: Boolean,
    val cull: CullFlag?,
    val rating: Int,
    val stackId: StackId? = null,
) {
    /** Whether the top of the rating scale has been reached. See [FAVOURITE_RATING]. */
    val isFavourite: Boolean get() = rating >= FAVOURITE_RATING
}

/**
 * Projects an asset onto the tile the grid draws.
 *
 * @param stackSize how many frames the asset's stack holds, when it has one. Passed in
 *   rather than looked up because a stack's size is a separate query and a cell must never
 *   be the thing that issues it — the caller batches one lookup per stack per window.
 */
fun Asset.toTimelineCell(stackSize: Int? = null): TimelineCell {
    val degradation = degradationOf()
    return TimelineCell(
        assetId = id,
        markers =
            GalleryCellMarkers(
                stackCount = stackSize?.takeIf { it > 1 },
                videoDuration = duration?.takeIf { isMovingImage() }?.let(::formatRunningTime),
                isFavourite = rating >= FAVOURITE_RATING,
                sync = syncState.toCellSync(),
                isDegraded = degradation != null,
            ),
        degradation = degradation,
        placeholder = (lqip?.dominantColor ?: NEUTRAL_PLACEHOLDER).toComposeColor(),
        capturedAt = captureTimestamp,
        isVideo = isMovingImage(),
        cull = cull.knownOrNull,
        rating = rating,
        stackId = stack?.stackId,
    )
}

/**
 * Which degraded state, if any, this asset is in.
 *
 * Ordered most-specific first and returning one value rather than a set: two markers on a
 * hundred-dp tile is two markers nobody reads, and the states genuinely nest — a quarantined
 * asset is also missing its original, and saying so twice adds nothing.
 */
private fun Asset.degradationOf(): TimelineDegradation? =
    when {
        syncState is SyncState.Quarantined -> TimelineDegradation.Quarantined
        // A content type outside the closed set is preserved verbatim rather than refused,
        // but nothing on this device claims it, so there is no decoder to reach for.
        contentType is Wire.Unknown -> TimelineDegradation.UnreadableOnThisDevice
        syncState is SyncState.AwaitingOriginal -> TimelineDegradation.AwaitingOriginal
        // Nothing above the embedded placeholder is held, so every fetch has already
        // degraded to the ladder's floor.
        !heldRepresentations.holds(RepresentationTier.Thumbnail) ->
            TimelineDegradation.FullResolutionUnavailable

        else -> null
    }

/**
 * What the corner sync marker should say.
 *
 * [SyncState.LocalOnly] draws nothing on purpose. In a library that has never had a server
 * it is the state of every asset, and a marker on every tile in the grid is chrome rather
 * than information — the Local Gallery contract makes never signing in a valid mode, not a
 * degradation to badge.
 */
private fun SyncState.toCellSync(): GalleryCellSync =
    when (this) {
        SyncState.Synced -> GalleryCellSync.Synced
        SyncState.LocalOnly -> GalleryCellSync.None
        is SyncState.Queued, is SyncState.Uploading -> GalleryCellSync.Pending
        SyncState.AwaitingOriginal -> GalleryCellSync.Pending
        is SyncState.Quarantined -> GalleryCellSync.Failed
        is SyncState.Failed -> GalleryCellSync.Failed
    }

/**
 * Whether this asset is a moving image.
 *
 * Read from the precise content type first and the coarse asset type second, because a
 * forward-schema asset carries an unknown `asset_type` token that must not drive behaviour
 * while its `content_type` may still be a format this build understands.
 */
private fun Asset.isMovingImage(): Boolean =
    contentType?.knownOrNull?.mediaKind == MediaKind.Video ||
        assetType.knownOrNull == AssetType.Video

/**
 * A running time as `m:ss`, or `h:mm:ss` once it passes the hour.
 *
 * Digits and colons only, so it needs no catalog entry and reads correctly under RTL — the
 * announced form beside it is the localized one.
 */
internal fun formatRunningTime(duration: Duration): String {
    val total = duration.seconds.coerceAtLeast(0)
    val hours = total / SECONDS_PER_HOUR
    val minutes = (total % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE
    val seconds = total % SECONDS_PER_MINUTE
    return if (hours > 0) {
        "$hours:${minutes.padded()}:${seconds.padded()}"
    } else {
        "$minutes:${seconds.padded()}"
    }
}

private fun Long.padded(): String = if (this < DOUBLE_DIGIT) "0$this" else toString()

/** The sidecar's dominant colour as Compose sees it. */
private fun RgbColor.toComposeColor(): Color = Color(red = red, green = green, blue = blue)

/** What a tile fills with when metadata carries no placeholder at all. */
private val NEUTRAL_PLACEHOLDER = RgbColor(NEUTRAL_LEVEL, NEUTRAL_LEVEL, NEUTRAL_LEVEL)

private const val NEUTRAL_LEVEL = 96
private const val SECONDS_PER_HOUR = 3_600L
private const val SECONDS_PER_MINUTE = 60L
private const val DOUBLE_DIGIT = 10L
