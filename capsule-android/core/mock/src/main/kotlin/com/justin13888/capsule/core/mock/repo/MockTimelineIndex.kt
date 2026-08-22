package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.TimelineIndex
import com.justin13888.capsule.core.mock.seed.dayBucketsOf
import com.justin13888.capsule.core.model.AspectRatioCodec
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.DayBucket
import com.justin13888.capsule.core.model.TimelineGeometry

/**
 * The timeline's geometry, with none of its content.
 *
 * Built by walking the ordered asset list once and keeping two flat arrays. The whole point is
 * that the gallery never has to hold an asset record to know where a tile goes, so this type
 * deliberately keeps no reference to the assets it was built from — a snapshot that quietly
 * retained forty thousand records would defeat the exercise.
 *
 * Both accessors delegate to [TimelineGeometry] rather than reimplementing the binary search
 * and the out-of-range fallback. Every adapter getting those boundaries wrong in its own
 * different way is worse than one shared implementation getting them wrong once.
 */
class MockTimelineIndex(
    override val days: List<DayBucket>,
    override val bucketOffsets: IntArray,
    override val aspects: ByteArray,
) : TimelineIndex {
    override val assetCount: Int get() = aspects.size

    override fun dayOf(assetIndex: Int): Int = TimelineGeometry.dayIndexFor(bucketOffsets, assetIndex)

    override fun aspectAt(assetIndex: Int): Float = TimelineGeometry.aspectAt(aspects, assetIndex)

    companion object {
        /**
         * Builds the index for [ordered], which must already be in timeline order.
         *
         * The arrays are freshly allocated on every build, which is what makes them safe to
         * hand to a layout pass on another thread: a shared array mutated underneath one would
         * corrupt every offset after the edit, and the contract on
         * [TimelineIndex.bucketOffsets] promises a snapshot.
         */
        fun of(ordered: List<Asset>): MockTimelineIndex {
            val days = dayBucketsOf(ordered)
            return MockTimelineIndex(
                days = days,
                bucketOffsets = TimelineGeometry.bucketOffsets(days),
                aspects = ByteArray(ordered.size) { AspectRatioCodec.quantize(ordered[it].dimensions) },
            )
        }
    }
}
