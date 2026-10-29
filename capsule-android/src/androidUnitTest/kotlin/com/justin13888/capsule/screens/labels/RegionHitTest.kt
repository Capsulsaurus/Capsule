package com.justin13888.capsule.screens.labels

import androidx.compose.ui.geometry.Offset
import com.justin13888.capsule.screens.domain.RegionShape
import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/** A tap selects the region it lands in, with a circle's radius taken of the shorter side. */
class RegionHitTest {
    @Test
    fun aRectangleContainsTapsInsideItsFractions() {
        val rect = RegionShape.Rect(0.25f, 0.5f, 0.5f, 0.25f)
        assertTrue(contains(rect, Offset(150f, 120f), width = 300f, height = 200f))
        assertFalse(contains(rect, Offset(50f, 120f), width = 300f, height = 200f))
    }

    @Test
    fun aCirclesRadiusIsAFractionOfTheShorterSide() {
        // On 300 x 200 the radius 0.25 is 50 px, not 75.
        val circle = RegionShape.Circle(0.5f, 0.5f, 0.25f)
        assertTrue(contains(circle, Offset(150f + 49f, 100f), width = 300f, height = 200f))
        assertFalse(contains(circle, Offset(150f + 60f, 100f), width = 300f, height = 200f))
    }

    @Test
    fun nothingIsHitBeforeLayout() {
        assertFalse(contains(RegionShape.Rect(0f, 0f, 1f, 1f), Offset.Zero, width = 0f, height = 0f))
    }
}
