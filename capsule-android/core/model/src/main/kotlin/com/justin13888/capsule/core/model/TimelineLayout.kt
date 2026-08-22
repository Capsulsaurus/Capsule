package com.justin13888.capsule.core.model

import java.time.LocalDate

/**
 * One day's run of assets in the timeline.
 *
 * The timeline is ordered newest first and a day's assets are contiguous within it, so a day
 * is fully described by where it starts and how long it is. Storing the run rather than the
 * ids is what lets the layout engine place a section header without holding any asset: it
 * needs the geometry, not the content.
 */
data class DayBucket(
    /** The local calendar day this run covers. */
    val day: LocalDate,
    /** The index of this day's first asset in the timeline ordering. */
    val startOffset: Int,
    /** How many assets fall on this day. */
    val count: Int,
) {
    /** One past this day's last asset — the next day's [startOffset]. */
    val endOffsetExclusive: Int get() = startOffset + count

    /** The stable `yyyy-MM-dd` key a section header is identified by. */
    val key: String get() = day.toString()
}

/**
 * The pure geometry the gallery's layout pass runs on.
 *
 * These functions live in the domain module rather than behind the repository contract on
 * purpose. They are arithmetic over two flat arrays with no I/O and no dependency on where
 * those arrays came from, so putting them here makes them testable in isolation and keeps
 * every adapter — the mock today, the UniFFI binding later — from writing its own binary
 * search and getting the boundary wrong in a different way.
 */
object TimelineGeometry {
    /** Returned when an index falls outside the timeline. */
    const val NO_DAY = -1

    /**
     * Builds the offsets array from a day list, appending the trailing sentinel.
     *
     * The sentinel — one past the last asset — is what removes the special case at the end.
     * Without it every consumer needs a branch on the final bucket, and one of them
     * eventually gets it wrong.
     */
    fun bucketOffsets(days: List<DayBucket>): IntArray {
        val offsets = IntArray(days.size + 1)
        days.forEachIndexed { index, bucket -> offsets[index] = bucket.startOffset }
        offsets[days.size] = days.lastOrNull()?.endOffsetExclusive ?: 0
        return offsets
    }

    /**
     * The index of the day containing [assetIndex], or [NO_DAY] when it is out of range.
     *
     * A binary search over the offsets rather than a stored per-asset day id: the day is
     * already implied by the offsets, and storing it again would be a second source of truth
     * able to disagree with them after an incremental update.
     *
     * Empty days are skipped rather than matched. A zero-length run contains no asset, so
     * returning it would hand the caller a bucket whose contents it cannot draw.
     */
    fun dayIndexFor(
        bucketOffsets: IntArray,
        assetIndex: Int,
    ): Int {
        val dayCount = bucketOffsets.size - 1
        if (dayCount <= 0 || assetIndex < bucketOffsets[0] || assetIndex >= bucketOffsets[dayCount]) {
            return NO_DAY
        }
        var low = 0
        var high = dayCount - 1
        while (low <= high) {
            val mid = (low + high) ushr 1
            when {
                assetIndex < bucketOffsets[mid] -> high = mid - 1
                assetIndex >= bucketOffsets[mid + 1] -> low = mid + 1
                else -> return mid
            }
        }
        return NO_DAY
    }

    /**
     * The decoded aspect ratio at [assetIndex], or a square when it is out of range.
     *
     * Out-of-range reads return a square rather than throwing because the layout pass races
     * the index: a scroll position computed against a slightly older index must not crash the
     * frame, and one square tile is a far cheaper failure than a dropped screen.
     */
    fun aspectAt(
        aspects: ByteArray,
        assetIndex: Int,
    ): Float =
        if (assetIndex in aspects.indices) {
            AspectRatioCodec.dequantize(aspects[assetIndex])
        } else {
            AspectRatioCodec.dequantize(AspectRatioCodec.SQUARE)
        }
}
