package com.justin13888.capsule.screens.domain

import kotlinx.coroutines.flow.Flow

/**
 * Labels on image regions, stacks and albums, and the regions themselves
 * (*Asset Organization - Labels*).
 *
 * Every operation is metadata-only: it rewrites a sidecar register and never touches an
 * original's bytes. Labels are added and removed **by text**: the adapter applies
 * [UserLabel.normalize] before anything is written, and a refused label writes nothing.
 *
 * Every write answers with what its target holds afterwards, so an editor redraws from
 * the store's truth rather than from what it asked for.
 *
 * Failures are [LabelRefusal], [ShapeRefusal] or [LabelTargetNotFound]; anything else is
 * the adapter being unreachable.
 */
interface LabelPort {
    /** The labels the target carries, sorted. Throws [LabelTargetNotFound]. */
    suspend fun labels(target: LabelTarget): List<String>

    /** Add a label, and return the target's labels afterwards. Nothing is written on a refusal. */
    suspend fun addLabel(
        label: String,
        target: LabelTarget,
    ): List<String>

    /**
     * Remove a label, and return the target's labels afterwards. A label the target does
     * not carry is not an error: the answer is the unchanged list.
     */
    suspend fun removeLabel(
        label: String,
        target: LabelTarget,
    ): List<String>

    /** The asset's live regions, oldest first. Throws [LabelTargetNotFound]. */
    suspend fun regions(asset: AssetId): List<AssetRegion>

    /**
     * Mark a region on an asset, with its first labels. Every label is checked before
     * the region is written, so a refused label leaves no unlabelled region behind.
     */
    suspend fun addRegion(
        shape: RegionShape,
        labels: List<String>,
        asset: AssetId,
    ): AssetRegion

    /** Delete a region. **Final**: it absorbs every concurrent edit and its id is never reused. */
    suspend fun deleteRegion(
        region: RegionId,
        asset: AssetId,
    )
}

/**
 * The live transfer event stream: every step an upload, a custody check, or a ranged
 * fetch takes, for the real-time transfer view.
 *
 * It is advisory - a transfer never waits on its observer, so a slow consumer reads
 * [TransferFeedItem.Lagged] with the count it missed, and the transfer carries on.
 */
fun interface TransferEventPort {
    /**
     * Every event published from now on, each stamped with the milliseconds since this
     * subscription opened on a monotonic clock. Cancelling the collector ends the
     * subscription and nothing else.
     */
    fun events(): Flow<TransferFeedItem>
}
