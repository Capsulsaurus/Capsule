package com.justin13888.capsule.feature.storage

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.knownOrNull
import java.time.Instant

/**
 * How much room one asset's locally-held rungs take.
 *
 * An estimate from the frame's dimensions rather than a stored byte count, because the
 * catalog does not carry one: [Asset] describes what a photograph *is*, not how large its
 * ciphertext turned out. The estimate is only ever used to *rank* consumers, which is what
 * the user is actually asking for — "what is worth clearing first" — and a ranking survives
 * a constant factor being wrong where a headline byte figure would not.
 *
 * Pure, so the ordering the screen shows is assertable without a device or a filesystem.
 */
object LocalFootprint {
    /** Bytes per pixel of a compressed still, as an eighth-of-a-byte ratio. */
    private const val ORIGINAL_NUMERATOR = 3L
    private const val ORIGINAL_DENOMINATOR = 8L

    /** A preview is capped at a screen-sized long edge, so its cost is roughly constant. */
    private const val PREVIEW_BYTES = 480L * 1024

    /** A grid thumbnail at ~256 px long edge. */
    private const val THUMBNAIL_BYTES = 24L * 1024

    /** The placeholder rides inside the metadata blob, so it costs no separate storage. */
    private const val LQIP_BYTES = 0L

    /** Video keeps far more bytes per frame than a still of the same dimensions. */
    private const val VIDEO_MULTIPLIER = 4L

    /** What [tier] costs for [asset] locally. */
    fun bytesOf(
        asset: Asset,
        tier: RepresentationTier,
    ): Long =
        when (tier) {
            RepresentationTier.Lqip -> LQIP_BYTES
            RepresentationTier.Thumbnail -> THUMBNAIL_BYTES
            RepresentationTier.Preview -> PREVIEW_BYTES
            RepresentationTier.Original -> originalBytes(asset)
        }

    /** What every rung this device currently holds costs together. */
    fun bytesOf(asset: Asset): Long = asset.heldRepresentations.tiers.sumOf { bytesOf(asset, it) }

    private fun originalBytes(asset: Asset): Long {
        val dimensions = asset.dimensions ?: return PREVIEW_BYTES
        val pixels = dimensions.width.toLong() * dimensions.height.toLong()
        val still = pixels * ORIGINAL_NUMERATOR / ORIGINAL_DENOMINATOR
        return if (asset.assetType.knownOrNull == AssetType.Video) still * VIDEO_MULTIPLIER else still
    }
}

/** What a release action is currently doing to one asset's local original. */
enum class ReleaseState {
    /** Nothing has been asked for. */
    Idle,

    /** The durability check is in flight. Nothing has been touched. */
    Checking,

    /**
     * The server did not confirm it holds the original, so the local copy was kept.
     *
     * A first-class state rather than an error toast: the whole point of the
     * verify-before-destroy gate is that the client keeps the bytes and *says so*, instead of
     * releasing optimistically and discovering later that it held the only copy.
     */
    Unconfirmed,

    /** The server confirmed it, and the local copy is now reclaimable. */
    Released,
}

/**
 * One row of "biggest on this device".
 *
 * Carries the asset's own facts and the decisions already taken about it, so the row renders
 * without reaching back into the repositories mid-composition.
 */
@Immutable
data class StorageConsumer(
    /** Which asset. */
    val id: AssetId,
    /** When it was captured — the only human handle the catalog offers for a frame. */
    val capturedAt: Instant,
    /** Still or moving image, or unknown to this build. */
    val assetType: AssetType?,
    /** The estimated local footprint of every rung held here. */
    val bytes: Long,
    /** Which rungs this device holds. */
    val held: HeldRepresentations,
    /** Whether the user pinned it for offline use. */
    val isPinned: Boolean,
    /** Where a release attempt on its original stands. */
    val release: ReleaseState,
) {
    /**
     * Whether this row holds the original bytes and can therefore be released.
     *
     * Only originals go through the durability gate. A thumbnail or a preview is rebuildable
     * or re-fetchable by definition, so the cache budget may evict it without asking anyone.
     */
    val holdsOriginal: Boolean get() = held.holds(RepresentationTier.Original)
}
