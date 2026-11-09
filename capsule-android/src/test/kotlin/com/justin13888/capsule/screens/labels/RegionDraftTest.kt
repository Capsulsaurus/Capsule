package com.justin13888.capsule.screens.labels

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertTrue

/** Every edit keeps a draft inside the shape contract the core validates, so whatever the user drew is writable. */
class RegionDraftTest {
    private fun isValid(draft: RegionDraft): Boolean = runCatching { draft.shape.validate() }.isSuccess

    @Test
    fun aNewDraftSitsInTheMiddleOfTheImageAndIsWritable() {
        assertEquals(RegionDraft.Circle(0.5, 0.5, 0.16), RegionDraft.of(RegionDraft.Kind.CIRCLE))
        assertEquals(RegionDraft.Rect(0.34, 0.34, 0.32, 0.32), RegionDraft.of(RegionDraft.Kind.RECT))
        assertTrue(RegionDraft.Kind.entries.all { isValid(RegionDraft.of(it)) })
    }

    @Test
    fun aDragPastAnEdgeStopsAtTheEdge() {
        val rect = RegionDraft.of(RegionDraft.Kind.RECT).moved(5.0, -5.0)
        assertEquals(RegionDraft.Rect(1 - 0.32, 0.0, 0.32, 0.32), rect)
        assertTrue(isValid(rect))
        val circle = RegionDraft.of(RegionDraft.Kind.CIRCLE).moved(-5.0, 5.0)
        assertEquals(RegionDraft.Circle(0.16, 0.84, 0.16), circle)
        assertTrue(isValid(circle))
    }

    @Test
    fun aResizeNeverShrinksBelowTheMinimumOrGrowsPastTheImage() {
        val small = RegionDraft.of(RegionDraft.Kind.RECT).resized(-1.0, -1.0)
        assertEquals(RegionDraft.Rect(0.34, 0.34, RegionDraft.MINIMUM_EXTENT, RegionDraft.MINIMUM_EXTENT), small)
        val large = RegionDraft.of(RegionDraft.Kind.RECT).resized(1.0, 1.0)
        assertEquals(RegionDraft.Rect(0.34, 0.34, 1 - 0.34, 1 - 0.34), large)
        assertTrue(isValid(large))
    }

    @Test
    fun aCircleGrowsOnlyUntilItMeetsTheNearestEdgeOnEitherAxis() {
        val offCentre = RegionDraft.Circle(0.3, 0.5, 0.1)
        assertEquals(RegionDraft.Circle(0.3, 0.5, 0.3), offCentre.resized(1.0, 0.0))
        // The larger of the two steps applies.
        val shrunk = assertIs<RegionDraft.Circle>(offCentre.resized(0.01, -0.05))
        assertEquals(0.05, shrunk.radius, 1e-12)
        assertTrue(isValid(offCentre.resized(1.0, 1.0)))
    }

    @Test
    fun aPinchScalesAboutTheCentreAndStaysInsideTheImage() {
        val rect = assertIs<RegionDraft.Rect>(RegionDraft.Rect(0.4, 0.4, 0.2, 0.2).scaled(2.0))
        assertEquals(0.3, rect.left, 1e-9)
        assertEquals(0.3, rect.top, 1e-9)
        assertEquals(0.4, rect.width, 1e-9)
        assertEquals(0.4, rect.height, 1e-9)
        assertTrue(isValid(RegionDraft.of(RegionDraft.Kind.RECT).scaled(100.0)))
        assertTrue(isValid(RegionDraft.of(RegionDraft.Kind.CIRCLE).scaled(100.0)))
        val circle = RegionDraft.of(RegionDraft.Kind.CIRCLE)
        assertEquals(circle, circle.scaled(Double.NaN))
        assertEquals(circle, circle.scaled(0.0))
    }

    @Test
    fun aShapeMeetingAnEdgeExactlyIsShavedToFitInSinglePrecision() {
        // Fractions that sum to exactly 1 in Double can round past 1 in Float.
        for (step in 0..98) {
            val left = step / 100.0
            RegionDraft.Rect(left, left, 1 - left, 1 - left).shape.validate()
        }
        for (step in 2..98) {
            val centre = step / 100.0
            RegionDraft.Circle(centre, 1 - centre, minOf(centre, 1 - centre)).shape.validate()
        }
    }
}
