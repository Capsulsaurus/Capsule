package com.justin13888.capsule.core.model

import kotlin.math.exp
import kotlin.math.ln
import kotlin.math.roundToInt

/**
 * Packs an aspect ratio into one byte, and back.
 *
 * The gallery has to lay out a million assets exactly — every row height, every scroll
 * offset — without loading a single pixel of content, and it has to do it before any
 * thumbnail request is issued. One byte per asset makes that a 1 MB array for a million-asset
 * library, small enough to hold resident and to hand to the layout pass as a flat
 * `ByteArray`; storing a `Float` instead would cost four times that and buy nothing, because
 * layout rounds to whole pixels anyway.
 *
 * The scale is **logarithmic** rather than linear because aspect ratio is perceptually
 * multiplicative: 16:9 and 9:16 are equally far from square, and a linear scale would spend
 * most of its buckets on the landscape half and starve the portrait half. Over
 * [MIN_RATIO]–[MAX_RATIO] with [BUCKETS] buckets the worst-case relative error is about
 * 0.55%, well under a pixel on any tile a phone draws.
 *
 * Ratios outside the range are clamped, not rejected: a 10:1 panorama is real content, and
 * refusing to lay it out would be worse than laying it out at 4:1 and letting the tile crop.
 */
object AspectRatioCodec {
    /** The narrowest representable ratio (tall portrait). Anything narrower clamps to it. */
    const val MIN_RATIO = 0.25f

    /** The widest representable ratio (wide landscape). Anything wider clamps to it. */
    const val MAX_RATIO = 4.0f

    /** The number of buckets — one per distinct byte value. */
    const val BUCKETS = 256

    private const val MAX_INDEX = BUCKETS - 1
    private const val BYTE_MASK = 0xFF

    private val LOG_MIN = ln(MIN_RATIO.toDouble())
    private val LOG_SPAN = ln(MAX_RATIO.toDouble()) - LOG_MIN

    /**
     * The bucket a square lands in, and the fallback for an unusable input.
     *
     * Declared after the log constants it is computed from — an object initialises top to
     * bottom, and reading them earlier would silently bake in a zero span.
     */
    val SQUARE: Byte = packRatio(1.0f)

    /**
     * Packs [ratio] (width ÷ height) into a bucket index stored as a byte.
     *
     * A non-finite or non-positive ratio yields [SQUARE]. That is the same fallback the
     * reference clients use for an asset whose dimensions the extractor could not probe: a
     * square tile is always a defensible guess, whereas propagating a `NaN` into the layout
     * pass corrupts every offset after it.
     */
    fun quantize(ratio: Float): Byte = if (!ratio.isFinite() || ratio <= 0f) SQUARE else packRatio(ratio)

    /** Packs [dimensions] directly, falling back to [SQUARE] when absent or degenerate. */
    fun quantize(dimensions: Dimensions?): Byte =
        if (dimensions == null || dimensions.width <= 0 || dimensions.height <= 0) {
            SQUARE
        } else {
            quantize(dimensions.aspectRatio)
        }

    /** Unpacks a byte written by [quantize] back into a ratio in [MIN_RATIO]–[MAX_RATIO]. */
    fun dequantize(packed: Byte): Float {
        val position = byteToIndex(packed).toDouble() / MAX_INDEX
        return exp(LOG_MIN + position * LOG_SPAN).toFloat()
    }

    /**
     * The bucket index a byte carries, `0..255`.
     *
     * A `Byte` is signed on the JVM, so the top half of the range arrives negative; masking
     * is what lets callers treat the array as a plain unsigned byte buffer.
     */
    fun byteToIndex(packed: Byte): Int = packed.toInt() and BYTE_MASK

    private fun packRatio(ratio: Float): Byte {
        val clamped = ratio.coerceIn(MIN_RATIO, MAX_RATIO).toDouble()
        val position = (ln(clamped) - LOG_MIN) / LOG_SPAN
        return (position * MAX_INDEX).roundToInt().coerceIn(0, MAX_INDEX).toByte()
    }
}
