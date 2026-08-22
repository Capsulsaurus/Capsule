package com.justin13888.capsule.core.mock

import app.cash.turbine.test
import com.justin13888.capsule.core.model.AspectRatioCodec
import com.justin13888.capsule.core.model.TimelineGeometry
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The contract the gallery engine consumes: two flat arrays that agree with each other.
 *
 * The trailing sentinel is the part worth guarding. It exists so no consumer needs a branch on
 * the final bucket, and an index that omitted it would look correct for every day but the last
 * one — which is the day a user scrolled to the bottom lands on.
 */
class TimelineIndexTest {
    private val repositories = MockFixtures.freshRepositories()

    @Test
    fun `bucket offsets are ascending, start at zero, and end at the asset count`() =
        runTest {
            repositories.timelineIndex.timelineIndex().test {
                val index = awaitItem()
                assertEquals(index.days.size + 1, index.bucketOffsets.size)
                assertEquals(0, index.bucketOffsets.first())
                // The sentinel: one past the last asset, which is also the total count.
                assertEquals(index.aspects.size, index.bucketOffsets.last())
                assertEquals(index.aspects.size, index.assetCount)
                assertTrue(
                    index.bucketOffsets
                        .asList()
                        .zipWithNext()
                        .all { (a, b) -> b > a },
                    "offsets are not strictly ascending",
                )
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `each day's run matches the bucket it describes`() =
        runTest {
            repositories.timelineIndex.timelineIndex().test {
                val index = awaitItem()
                index.days.forEachIndexed { position, day ->
                    assertEquals(day.startOffset, index.bucketOffsets[position], "day ${day.key} start")
                    assertEquals(day.endOffsetExclusive, index.bucketOffsets[position + 1], "day ${day.key} end")
                    assertTrue(day.count > 0, "empty day ${day.key} was emitted as a bucket")
                }
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `dayOf resolves every in-range index and refuses every out-of-range one`() =
        runTest {
            repositories.timelineIndex.timelineIndex().test {
                val index = awaitItem()
                assertEquals(0, index.dayOf(0))
                assertEquals(index.days.lastIndex, index.dayOf(index.assetCount - 1))
                assertEquals(TimelineGeometry.NO_DAY, index.dayOf(-1))
                assertEquals(TimelineGeometry.NO_DAY, index.dayOf(index.assetCount))
                // Every boundary, not a sample: an off-by-one at a run edge is the whole risk.
                index.days.forEachIndexed { position, day ->
                    assertEquals(position, index.dayOf(day.startOffset))
                    assertEquals(position, index.dayOf(day.endOffsetExclusive - 1))
                }
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `aspects decode to the ratios the assets actually have`() =
        runTest {
            val window = repositories.timeline.timelineWindow(offset = 0, limit = 64)
            repositories.timelineIndex.timelineIndex().test {
                val index = awaitItem()
                window.forEachIndexed { position, asset ->
                    val expected = AspectRatioCodec.dequantize(AspectRatioCodec.quantize(asset.dimensions))
                    assertEquals(expected, index.aspectAt(position), TOLERANCE)
                }
                // Out of range returns a square rather than throwing: the layout pass races the
                // index, and one square tile is a far cheaper failure than a dropped frame.
                assertEquals(
                    AspectRatioCodec.dequantize(AspectRatioCodec.SQUARE),
                    index.aspectAt(index.assetCount + 1),
                    TOLERANCE,
                )
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an album index is a strict subset of the library index`() =
        runTest {
            val albumId =
                MockFixtures.LIBRARY.landmarks.tripAlbumIds
                    .first()
            repositories.timelineIndex.albumIndex(albumId).test {
                val index = awaitItem()
                assertTrue(index.assetCount > 0)
                assertEquals(index.aspects.size, index.bucketOffsets.last())
                assertTrue(index.assetCount < MockFixtures.LIBRARY.assets.size)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the index re-emits after a write that changes what a default view shows`() =
        runTest {
            val target = repositories.timeline.timelineWindow(offset = 0, limit = 1).single()
            repositories.timelineIndex.timelineIndex().test {
                val before = awaitItem().assetCount
                repositories.assets.setHidden(target.id, hidden = true)
                assertEquals(before - 1, awaitItem().assetCount)
                cancelAndIgnoreRemainingEvents()
            }
        }

    private companion object {
        const val TOLERANCE = 0.0001f
    }
}
