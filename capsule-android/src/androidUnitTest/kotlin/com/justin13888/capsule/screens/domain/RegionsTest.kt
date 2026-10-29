package com.justin13888.capsule.screens.domain

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

/**
 * The label rule and the region shape contract, which mirror the core's
 * `normalize_label` and `RegionShape::validate` so an editor refuses before it sends
 * what the core would refuse on write.
 */
class RegionsTest {
    @Test
    fun aLabelIsNfcNormalizedAndTrimmedAndKeepsItsCase() {
        // "e" followed by a combining acute accent composes to one code point.
        val label = UserLabel.normalize("  Café Trip\n")
        assertEquals("Café Trip", label)
        assertEquals(9, label.codePointCount(0, label.length))
    }

    @Test
    fun aLabelOfOnlyWhitespaceIsRefusedAsEmpty() {
        assertFailsWith<LabelRefusal.Empty> { UserLabel.normalize(" \t\n ") }
    }

    @Test
    fun aLabelIsBoundedInUtf8BytesNotCharacters() {
        val atBound = "a".repeat(UserLabel.MAX_BYTES)
        assertEquals(atBound, UserLabel.normalize(atBound))
        // 129 two-byte characters: 129 characters, 258 bytes.
        val refusal = assertFailsWith<LabelRefusal.TooLong> { UserLabel.normalize("é".repeat(129)) }
        assertEquals(258, refusal.bytes)
    }

    @Test
    fun aShapeInsideTheImageValidates() {
        RegionShape.Circle(0.5f, 0.5f, 0.5f).validate()
        RegionShape.Rect(0f, 0f, 1f, 1f).validate()
        RegionShape.Rect(0.25f, 0.1f, 0.5f, 0.2f).validate()
    }

    @Test
    fun aCoordinateOutsideTheUnitIntervalOrNotFiniteIsOutOfRange() {
        assertRefused(ShapeRefusalReason.OUT_OF_RANGE, RegionShape.Rect(-0.1f, 0f, 0.5f, 0.5f))
        assertRefused(ShapeRefusalReason.OUT_OF_RANGE, RegionShape.Circle(Float.NaN, 0.5f, 0.1f))
        assertRefused(ShapeRefusalReason.OUT_OF_RANGE, RegionShape.Circle(0.5f, 0.5f, Float.POSITIVE_INFINITY))
    }

    @Test
    fun aShapeWithNoAreaIsEmpty() {
        assertRefused(ShapeRefusalReason.EMPTY, RegionShape.Circle(0.5f, 0.5f, 0f))
        assertRefused(ShapeRefusalReason.EMPTY, RegionShape.Rect(0.1f, 0.1f, 0.2f, 0f))
    }

    @Test
    fun aShapePastAnEdgeIsRefusedAndACircleMustFitItsRadiusOnBothAxes() {
        assertRefused(ShapeRefusalReason.PAST_EDGE, RegionShape.Rect(0.6f, 0f, 0.5f, 0.5f))
        assertRefused(ShapeRefusalReason.PAST_EDGE, RegionShape.Circle(0.2f, 0.5f, 0.3f))
        assertRefused(ShapeRefusalReason.PAST_EDGE, RegionShape.Circle(0.5f, 0.9f, 0.2f))
    }

    private fun assertRefused(
        reason: ShapeRefusalReason,
        shape: RegionShape,
    ) {
        assertEquals(reason, assertFailsWith<ShapeRefusal> { shape.validate() }.reason)
    }
}
