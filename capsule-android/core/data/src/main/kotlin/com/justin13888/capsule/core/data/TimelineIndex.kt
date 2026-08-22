package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AspectRatioCodec
import com.justin13888.capsule.core.model.DayBucket
import com.justin13888.capsule.core.model.TimelineGeometry
import kotlinx.coroutines.flow.Flow

/**
 * The geometry of the whole timeline, with none of its content.
 *
 * A gallery over a million assets has to know where every tile goes *before* it can decide
 * which tiles to load — scroll-bar extent, section-header positions, and the mapping from a
 * scroll offset back to an index all depend on the full layout. Computing that from asset
 * records would mean holding a million of them resident; computing it from this contract
 * costs two flat arrays and no object allocation at all.
 *
 * The two arrays are deliberately primitive rather than lists of a domain type. At a million
 * assets the difference is roughly 1 MB of `ByteArray` against tens of megabytes of boxed
 * objects, and the layout pass walks them linearly, which is exactly the access pattern a
 * flat array is good at.
 *
 * Implementations return **defensive copies or immutable snapshots**: the arrays are shared
 * with a layout pass that may run on another thread, and a mutation underneath it would
 * corrupt every offset after the edit.
 */
interface TimelineIndex {
    /**
     * Per-day start offsets into the timeline ordering, ascending, with a trailing sentinel.
     *
     * `bucketOffsets[i]` is the index of day `i`'s first asset and `bucketOffsets[i + 1]` is
     * one past its last, so the array has `dayCount + 1` entries and a day's length is a
     * subtraction rather than a lookup. The sentinel is what removes the special case at the
     * end — without it every consumer would need a branch on the last bucket, and one of them
     * would eventually get it wrong.
     */
    val bucketOffsets: IntArray

    /**
     * One byte per asset, in timeline order, holding its quantized aspect ratio.
     *
     * Encoded and decoded by [AspectRatioCodec] — log-scale over `0.25..4.0`, so a portrait
     * and a landscape shot of the same extremity get the same precision. `aspects.size` is
     * the timeline's total asset count.
     */
    val aspects: ByteArray

    /** The day each entry of [bucketOffsets] describes, in the same order. */
    val days: List<DayBucket>

    /** Total assets in the timeline — the same number as `aspects.size`. */
    val assetCount: Int

    /**
     * The index of the day containing [assetIndex], or [TimelineGeometry.NO_DAY] when it is
     * out of range.
     *
     * Implementations delegate to [TimelineGeometry.dayIndexFor] rather than rolling their
     * own binary search — the boundary conditions are easy to get subtly wrong, and every
     * adapter getting them wrong differently is worse than any one of them getting them
     * wrong once.
     */
    fun dayOf(assetIndex: Int): Int

    /**
     * The decoded aspect ratio at [assetIndex], or a square when it is out of range.
     *
     * Implementations delegate to [TimelineGeometry.aspectAt].
     */
    fun aspectAt(assetIndex: Int): Float
}

/**
 * The source of a live [TimelineIndex].
 *
 * A [Flow] rather than a one-shot because the index changes under the user: an import lands,
 * a sync page applies, an asset is trashed. Recomputing on every scroll frame would be
 * absurd, and polling would be worse, so the layout observes it and re-lays out only when it
 * actually changes.
 */
interface TimelineIndexSource {
    /** The current index, re-emitted whenever the underlying catalog changes it. */
    fun timelineIndex(): Flow<TimelineIndex>

    /** The index restricted to one album, for an album's own grid. */
    fun albumIndex(albumId: AlbumId): Flow<TimelineIndex>
}
