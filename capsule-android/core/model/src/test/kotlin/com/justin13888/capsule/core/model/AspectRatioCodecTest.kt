package com.justin13888.capsule.core.model

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.params.ParameterizedTest
import org.junit.jupiter.params.provider.ValueSource
import kotlin.math.abs

/**
 * The one-byte aspect encoding the gallery lays a million assets out from.
 *
 * The accuracy bound is asserted rather than assumed: the whole justification for spending
 * one byte instead of four is that the resulting error is invisible, and a regression that
 * widened it would show up as tiles that do not line up with the images they eventually load.
 */
class AspectRatioCodecTest {
    @ParameterizedTest
    @ValueSource(
        floats = [
            0.25f, 0.3f, 0.5f, 0.5625f, 0.6667f, 0.75f, 0.8f, 1.0f,
            1.25f, 1.3333f, 1.5f, 1.7778f, 2.0f, 2.35f, 3.0f, 4.0f,
        ],
    )
    fun `round trip stays within the documented accuracy bound`(ratio: Float) {
        val decoded = AspectRatioCodec.dequantize(AspectRatioCodec.quantize(ratio))
        val relativeError = abs(decoded - ratio) / ratio
        assertTrue(
            relativeError <= MAX_RELATIVE_ERROR,
            "ratio $ratio decoded as $decoded — relative error $relativeError exceeds $MAX_RELATIVE_ERROR",
        )
    }

    @Test
    fun `worst-case error across the whole range stays under the bound`() {
        // Sweeping the range catches a bucket-edge regression that a handful of round
        // numbers would walk straight past.
        var worst = 0.0f
        var steps = 0
        var ratio = AspectRatioCodec.MIN_RATIO
        while (ratio <= AspectRatioCodec.MAX_RATIO) {
            val decoded = AspectRatioCodec.dequantize(AspectRatioCodec.quantize(ratio))
            worst = maxOf(worst, abs(decoded - ratio) / ratio)
            ratio *= SWEEP_STEP
            steps++
        }
        assertTrue(steps > MIN_SWEEP_STEPS, "the sweep covered only $steps points")
        assertTrue(worst <= MAX_RELATIVE_ERROR, "worst relative error was $worst")
    }

    @Test
    fun `the encoding is monotonic across the range`() {
        // Layout depends on ordering as much as on magnitude: a non-monotonic bucket would
        // put a wider image in a narrower slot.
        val indices =
            generateSequence(AspectRatioCodec.MIN_RATIO) { it * SWEEP_STEP }
                .takeWhile { it <= AspectRatioCodec.MAX_RATIO }
                .map { AspectRatioCodec.byteToIndex(AspectRatioCodec.quantize(it)) }
                .toList()
        indices.zipWithNext { a, b -> assertTrue(b >= a, "index went backwards: $a then $b") }
    }

    @Test
    fun `the extremes land on the first and last buckets`() {
        assertEquals(0, AspectRatioCodec.byteToIndex(AspectRatioCodec.quantize(AspectRatioCodec.MIN_RATIO)))
        assertEquals(
            AspectRatioCodec.BUCKETS - 1,
            AspectRatioCodec.byteToIndex(AspectRatioCodec.quantize(AspectRatioCodec.MAX_RATIO)),
        )
    }

    @Test
    fun `out-of-range ratios clamp instead of being refused`() {
        // A 10:1 panorama is real content. Refusing to lay it out would be worse than laying
        // it out at the widest bucket and letting the tile crop.
        assertEquals(AspectRatioCodec.quantize(AspectRatioCodec.MAX_RATIO), AspectRatioCodec.quantize(10f))
        assertEquals(AspectRatioCodec.quantize(AspectRatioCodec.MIN_RATIO), AspectRatioCodec.quantize(0.05f))
    }

    @Test
    fun `a square is representable exactly enough to be recognisable`() {
        val decoded = AspectRatioCodec.dequantize(AspectRatioCodec.SQUARE)
        assertTrue(abs(decoded - 1f) / 1f <= MAX_RELATIVE_ERROR, "square decoded as $decoded")
    }

    @Test
    fun `unusable input falls back to square rather than propagating`() {
        // A NaN reaching the layout pass corrupts every offset after it, so the codec absorbs
        // it here where the damage is one tile instead of the whole scroll.
        assertEquals(AspectRatioCodec.SQUARE, AspectRatioCodec.quantize(Float.NaN))
        assertEquals(AspectRatioCodec.SQUARE, AspectRatioCodec.quantize(Float.POSITIVE_INFINITY))
        assertEquals(AspectRatioCodec.SQUARE, AspectRatioCodec.quantize(0f))
        assertEquals(AspectRatioCodec.SQUARE, AspectRatioCodec.quantize(-1.5f))
    }

    @Test
    fun `absent or degenerate dimensions fall back to square`() {
        assertEquals(AspectRatioCodec.SQUARE, AspectRatioCodec.quantize(null))
        assertEquals(AspectRatioCodec.SQUARE, AspectRatioCodec.quantize(Dimensions(width = 0, height = 100)))
        assertEquals(AspectRatioCodec.SQUARE, AspectRatioCodec.quantize(Dimensions(width = 100, height = 0)))
    }

    @Test
    fun `dimensions encode the same as their derived ratio`() {
        val dimensions = Dimensions(width = 4032, height = 3024)
        assertEquals(AspectRatioCodec.quantize(dimensions.aspectRatio), AspectRatioCodec.quantize(dimensions))
    }

    @Test
    fun `every bucket is reachable and decodes back into range`() {
        // The byte is signed on the JVM; a masking bug would silently fold the top half of
        // the range onto the bottom.
        val reached =
            (0 until AspectRatioCodec.BUCKETS).map { index ->
                val decoded = AspectRatioCodec.dequantize(index.toByte())
                assertTrue(
                    decoded >= AspectRatioCodec.MIN_RATIO * (1 - TOLERANCE) &&
                        decoded <= AspectRatioCodec.MAX_RATIO * (1 + TOLERANCE),
                    "bucket $index decoded outside the range as $decoded",
                )
                AspectRatioCodec.byteToIndex(AspectRatioCodec.quantize(decoded))
            }
        assertEquals((0 until AspectRatioCodec.BUCKETS).toList(), reached)
    }

    private companion object {
        /** Half a bucket in log space over `0.25..4.0` with 256 buckets, plus float slack. */
        const val MAX_RELATIVE_ERROR = 0.006f
        const val SWEEP_STEP = 1.001f
        const val MIN_SWEEP_STEPS = 2000
        const val TOLERANCE = 1e-5f
    }
}
