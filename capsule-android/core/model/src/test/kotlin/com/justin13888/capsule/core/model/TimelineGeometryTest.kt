package com.justin13888.capsule.core.model

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.LocalDate
import kotlin.math.abs

/**
 * The arithmetic the gallery lays a million assets out with.
 *
 * Boundaries are the whole risk here — a day's first asset, its last, the sentinel, an index
 * one past the end — so nearly every case below sits on one. A layout engine that is off by
 * one at a section boundary produces a scroll position that drifts further wrong the longer
 * the user scrolls, which is exactly the class of bug that is impossible to diagnose from a
 * screenshot.
 */
class TimelineGeometryTest {
    private val days =
        listOf(
            DayBucket(LocalDate.of(2026, 7, 3), startOffset = 0, count = 3),
            DayBucket(LocalDate.of(2026, 7, 2), startOffset = 3, count = 1),
            DayBucket(LocalDate.of(2026, 6, 30), startOffset = 4, count = 5),
        )
    private val offsets = TimelineGeometry.bucketOffsets(days)

    @Test
    fun `the offsets array carries one entry per day plus a sentinel`() {
        assertEquals(days.size + 1, offsets.size)
        assertArrayEquals(intArrayOf(0, 3, 4, 9), offsets)
    }

    @Test
    fun `the sentinel is one past the last asset`() {
        assertEquals(days.sumOf(DayBucket::count), offsets.last())
    }

    @Test
    fun `an empty timeline still produces a well-formed sentinel`() {
        val empty = TimelineGeometry.bucketOffsets(emptyList())
        assertArrayEquals(intArrayOf(0), empty)
        assertEquals(TimelineGeometry.NO_DAY, TimelineGeometry.dayIndexFor(empty, 0))
    }

    @Test
    fun `every asset index resolves to the day that contains it`() {
        val expected = listOf(0, 0, 0, 1, 2, 2, 2, 2, 2)
        expected.forEachIndexed { assetIndex, day ->
            assertEquals(day, TimelineGeometry.dayIndexFor(offsets, assetIndex), "asset $assetIndex")
        }
    }

    @Test
    fun `the first and last asset of each day resolve to that day`() {
        days.forEachIndexed { dayIndex, bucket ->
            assertEquals(dayIndex, TimelineGeometry.dayIndexFor(offsets, bucket.startOffset))
            assertEquals(dayIndex, TimelineGeometry.dayIndexFor(offsets, bucket.endOffsetExclusive - 1))
        }
    }

    @Test
    fun `an index past the end resolves to no day`() {
        assertEquals(TimelineGeometry.NO_DAY, TimelineGeometry.dayIndexFor(offsets, 9))
        assertEquals(TimelineGeometry.NO_DAY, TimelineGeometry.dayIndexFor(offsets, 1000))
    }

    @Test
    fun `a negative index resolves to no day`() {
        assertEquals(TimelineGeometry.NO_DAY, TimelineGeometry.dayIndexFor(offsets, -1))
    }

    @Test
    fun `a zero-length day is never returned`() {
        // A day with no assets has nothing to draw, so handing it back would give the caller
        // a bucket it cannot render.
        val withEmpty =
            TimelineGeometry.bucketOffsets(
                listOf(
                    DayBucket(LocalDate.of(2026, 7, 3), startOffset = 0, count = 2),
                    DayBucket(LocalDate.of(2026, 7, 2), startOffset = 2, count = 0),
                    DayBucket(LocalDate.of(2026, 7, 1), startOffset = 2, count = 2),
                ),
            )
        assertEquals(0, TimelineGeometry.dayIndexFor(withEmpty, 1))
        assertEquals(2, TimelineGeometry.dayIndexFor(withEmpty, 2))
        assertEquals(2, TimelineGeometry.dayIndexFor(withEmpty, 3))
    }

    @Test
    fun `the search holds up over a large timeline`() {
        // The binary search is the only reason a million-asset library can map a scroll
        // offset back to a section without a linear scan.
        val start = LocalDate.of(2026, 1, 1)
        val bigDays = List(DAY_COUNT) { DayBucket(start.plusDays(it.toLong()), it * PER_DAY, PER_DAY) }
        val bigOffsets = TimelineGeometry.bucketOffsets(bigDays)
        assertEquals(DAY_COUNT * PER_DAY, bigOffsets.last())
        assertEquals(0, TimelineGeometry.dayIndexFor(bigOffsets, 0))
        assertEquals(DAY_COUNT / 2, TimelineGeometry.dayIndexFor(bigOffsets, DAY_COUNT / 2 * PER_DAY + 3))
        assertEquals(DAY_COUNT - 1, TimelineGeometry.dayIndexFor(bigOffsets, DAY_COUNT * PER_DAY - 1))
        assertEquals(TimelineGeometry.NO_DAY, TimelineGeometry.dayIndexFor(bigOffsets, DAY_COUNT * PER_DAY))
    }

    @Test
    fun `aspects decode back to what was packed`() {
        val ratios = floatArrayOf(0.75f, 1.0f, 1.5f, 1.7778f)
        val packed = ByteArray(ratios.size) { AspectRatioCodec.quantize(ratios[it]) }
        ratios.forEachIndexed { index, ratio ->
            val decoded = TimelineGeometry.aspectAt(packed, index)
            assertTrue(abs(decoded - ratio) / ratio <= MAX_RELATIVE_ERROR, "index $index decoded as $decoded")
        }
    }

    @Test
    fun `an out-of-range aspect read yields a square rather than throwing`() {
        // The layout pass races the index; a stale scroll position must not crash the frame.
        val packed = ByteArray(2) { AspectRatioCodec.quantize(1.5f) }
        val square = AspectRatioCodec.dequantize(AspectRatioCodec.SQUARE)
        assertEquals(square, TimelineGeometry.aspectAt(packed, 2))
        assertEquals(square, TimelineGeometry.aspectAt(packed, -1))
        assertEquals(square, TimelineGeometry.aspectAt(ByteArray(0), 0))
    }

    @Test
    fun `the aspect array is one byte per asset`() {
        // The size claim is the justification for the whole encoding: a million assets fit in
        // a megabyte, which is what makes holding the layout resident affordable.
        val packed = ByteArray(days.sumOf(DayBucket::count)) { AspectRatioCodec.SQUARE }
        assertEquals(offsets.last(), packed.size)
    }

    private companion object {
        const val DAY_COUNT = 4000
        const val PER_DAY = 25
        const val MAX_RELATIVE_ERROR = 0.006f
    }
}
