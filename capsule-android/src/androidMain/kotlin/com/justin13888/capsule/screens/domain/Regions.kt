package com.justin13888.capsule.screens.domain

import java.text.Normalizer

/**
 * The one label rule, at every level a label attaches to - asset, image region, stack
 * and album (*Asset Organization - Labels*).
 *
 * A label is NFC-normalized, trimmed of surrounding whitespace, 1 to [MAX_BYTES] UTF-8
 * bytes, and case-preserving. It carries no hierarchy and no type. Mirrors
 * `capsule_core::sidecar::normalize_label`, which is what the core refuses on write; an
 * editor checks here first so a refusal is shown before anything is sent.
 */
object UserLabel {
    /** The largest label, in UTF-8 bytes after normalization. */
    const val MAX_BYTES = 256

    /**
     * Normalize [raw] into a storable label: NFC, then trimmed, then checked against
     * the byte bound.
     *
     * @throws LabelRefusal.Empty when nothing is left once trimmed.
     * @throws LabelRefusal.TooLong when the normalized label is past [MAX_BYTES].
     */
    fun normalize(raw: String): String {
        val trimmed = Normalizer.normalize(raw, Normalizer.Form.NFC).trim()
        if (trimmed.isEmpty()) throw LabelRefusal.Empty()
        val bytes = trimmed.toByteArray(Charsets.UTF_8).size
        if (bytes > MAX_BYTES) throw LabelRefusal.TooLong(bytes)
        return trimmed
    }
}

/** Why a label was refused. Equal by kind and content, so tests and editors compare them. */
sealed class LabelRefusal(
    message: String,
) : IllegalArgumentException(message) {
    /** Nothing is left once the label is trimmed. */
    class Empty : LabelRefusal("label is empty") {
        override fun equals(other: Any?): Boolean = other is Empty

        override fun hashCode(): Int = javaClass.hashCode()
    }

    /** The normalized label is [bytes] UTF-8 bytes, past [UserLabel.MAX_BYTES]. */
    class TooLong(
        val bytes: Int,
    ) : LabelRefusal("label is $bytes bytes") {
        override fun equals(other: Any?): Boolean = other is TooLong && other.bytes == bytes

        override fun hashCode(): Int = bytes
    }
}

/** Why a shape was refused. */
enum class ShapeRefusalReason {
    /** A coordinate is not a finite number in `[0, 1]`. */
    OUT_OF_RANGE,

    /** A radius, width or height is zero. */
    EMPTY,

    /** A rectangle extends past the right or bottom edge, or a circle past any edge. */
    PAST_EDGE,
}

/** A shape outside the normalized-coordinate contract. */
class ShapeRefusal(
    val reason: ShapeRefusalReason,
) : IllegalArgumentException("region shape refused: $reason") {
    override fun equals(other: Any?): Boolean = other is ShapeRefusal && other.reason == reason

    override fun hashCode(): Int = reason.hashCode()
}

/**
 * A region's outline, in coordinates normalized to `[0, 1]` over the original's
 * **display orientation** (after EXIF orientation), so it survives every derivative
 * size.
 *
 * Mirrors `capsule_core::sidecar::RegionShape`, including its single-precision
 * coordinates: a shape that validates here validates in the core.
 */
sealed interface RegionShape {
    /**
     * A circle centred at ([centerX], [centerY]) (fractions of width and height) with a
     * radius that is a fraction of the image's **shorter** side, so it is round on any
     * aspect ratio. The core's `cx`, `cy` and `r`.
     */
    data class Circle(
        val centerX: Float,
        val centerY: Float,
        val radius: Float,
    ) : RegionShape

    /** A rectangle with its top-left corner at ([left], [top]). The core's `x`, `y`, `w`, `h`. */
    data class Rect(
        val left: Float,
        val top: Float,
        val width: Float,
        val height: Float,
    ) : RegionShape

    /**
     * Check the normalized-coordinate contract: every value finite and in `[0, 1]`, a
     * positive extent, and the shape wholly inside the image.
     *
     * A circle spans exactly its radius of the shorter axis and the shape does not
     * record which axis that is, so it must fit the radius from its centre on both.
     *
     * @throws ShapeRefusal naming the first rule the shape breaks.
     */
    fun validate() {
        val unit = { value: Float -> value.isFinite() && value in 0f..1f }
        when (this) {
            is Circle -> {
                refuseUnless(listOf(centerX, centerY, radius).all(unit), ShapeRefusalReason.OUT_OF_RANGE)
                refuseUnless(radius > 0f, ShapeRefusalReason.EMPTY)
                refuseUnless(
                    centerX >= radius && centerY >= radius && centerX + radius <= 1f && centerY + radius <= 1f,
                    ShapeRefusalReason.PAST_EDGE,
                )
            }
            is Rect -> {
                refuseUnless(listOf(left, top, width, height).all(unit), ShapeRefusalReason.OUT_OF_RANGE)
                refuseUnless(width > 0f && height > 0f, ShapeRefusalReason.EMPTY)
                refuseUnless(left + width <= 1f && top + height <= 1f, ShapeRefusalReason.PAST_EDGE)
            }
        }
    }
}

private fun refuseUnless(
    holds: Boolean,
    reason: ShapeRefusalReason,
) {
    if (!holds) throw ShapeRefusal(reason)
}

/**
 * One live region of an asset and its labels, sorted.
 *
 * A region's labels are an OR-set of their own, so two devices labelling one region
 * concurrently both keep their label; a delete is final and absorbs every concurrent
 * edit.
 */
data class AssetRegion(
    val id: RegionId,
    val shape: RegionShape,
    val labels: List<String>,
)

/**
 * What a label is attached to, beyond the asset itself. The label rule is the same at
 * every level.
 *
 * An asset's own labels are its user tags, edited where the other organization edits
 * are; they are not a target here, so there is one write path for them and not two.
 */
sealed interface LabelTarget {
    /** A region of an asset. */
    data class Region(
        val asset: AssetId,
        val region: RegionId,
    ) : LabelTarget

    /** A stack. */
    data class Stack(
        val stack: StackId,
    ) : LabelTarget

    /** An album. */
    data class Album(
        val album: AlbumId,
    ) : LabelTarget
}

/**
 * The asset, region, stack or album does not exist, or no longer does: a deleted
 * region's id is never live again.
 */
class LabelTargetNotFound : NoSuchElementException("label target not found") {
    override fun equals(other: Any?): Boolean = other is LabelTargetNotFound

    override fun hashCode(): Int = javaClass.hashCode()
}
