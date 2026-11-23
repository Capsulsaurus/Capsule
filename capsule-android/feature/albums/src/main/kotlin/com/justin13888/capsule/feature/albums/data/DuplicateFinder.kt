package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.feature.albums.model.DuplicateGroup
import com.justin13888.capsule.feature.albums.model.DuplicateKind
import java.time.Duration

/**
 * Finds candidate duplicates in a window of the library.
 *
 * Two passes, in order of confidence, because the two answers deserve different copy and
 * different defaults. The first is the plaintext content hash the catalog already
 * deduplicates imports on: equal hashes mean equal bytes, full stop. The second is a
 * conservative near-duplicate signature — same pixel dimensions, same placeholder colour,
 * captured within seconds of each other — which finds the re-export and the
 * "shared it and it came back" copy that a hash cannot.
 *
 * Deliberately pure and deliberately conservative. The screen it feeds offers to move
 * copies to trash, and a finder that over-grouped would turn one wrong guess into a batch
 * of soft deletes; every step past this point is reversible, but the user's confidence is
 * not.
 */
object DuplicateFinder {
    /** How far apart two captures may be and still be treated as the same moment. */
    val NEAR_DUPLICATE_WINDOW: Duration = Duration.ofSeconds(2)

    /**
     * Groups [assets] into duplicate candidates, most confident first.
     *
     * Ordering inside a group puts the asset that would be *kept* first: the one holding the
     * most of the representation ladder locally, then the earliest import, then the id. That
     * order is total, so a rescan of the same library produces the same suggestion — a
     * finder whose suggestion moved between runs would be one nobody could review.
     */
    fun find(assets: List<Asset>): List<DuplicateGroup> {
        val identical =
            assets
                .groupBy { it.contentHash.value }
                .filterValues { it.size > 1 }
        val identicalIds = identical.values.flatten().mapTo(mutableSetOf()) { it.id.value }
        val similar =
            assets
                .filterNot { it.id.value in identicalIds }
                .groupBy(::nearDuplicateSignature)
                .filterKeys { it != null }
                .filterValues { it.size > 1 }

        return buildList {
            identical.forEach { (hash, members) -> add(group("hash:$hash", DuplicateKind.Identical, members)) }
            similar.forEach { (signature, members) -> add(group("near:$signature", DuplicateKind.Similar, members)) }
        }
    }

    private fun group(
        id: String,
        kind: DuplicateKind,
        members: List<Asset>,
    ): DuplicateGroup = DuplicateGroup(id = id, kind = kind, members = members.sortedWith(KEEPER_ORDER))

    /**
     * The near-duplicate bucket an asset falls in, or `null` when it cannot be compared.
     *
     * An asset with no probed dimensions and no placeholder colour carries nothing to
     * compare, so it is left alone rather than grouped with every other unprobed file.
     */
    private fun nearDuplicateSignature(asset: Asset): String? {
        val dimensions = asset.dimensions ?: return null
        val colour = asset.lqip?.dominantColor ?: return null
        val bucket = asset.captureTimestamp.epochSecond / NEAR_DUPLICATE_WINDOW.seconds
        return "${dimensions.width}x${dimensions.height}@${colour.toHex()}#$bucket"
    }

    /** Best keeper first: most held locally, then earliest imported, then by id. */
    private val KEEPER_ORDER: Comparator<Asset> =
        compareByDescending<Asset> { it.heldRepresentations.tierCount() }
            .thenBy { it.importTimestamp }
            .thenBy { it.id.value }

    /** How many rungs of the representation ladder this device is holding. */
    private fun HeldRepresentations.tierCount(): Int = tiers.size
}
