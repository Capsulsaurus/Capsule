package com.justin13888.capsule.shell

import androidx.compose.material3.adaptive.HingeInfo
import androidx.compose.material3.adaptive.Posture
import androidx.compose.ui.geometry.Rect
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * Pins the classification the whole foldable story rests on.
 *
 * Every screen reads [FoldPosture] instead of the raw hinge list precisely so this decision
 * is made once. That only pays off if the decision is right, and the cases that matter are
 * the ones a device reports at the edges: a flat hinge that must *not* trigger a book layout,
 * and a device reporting both hinge orientations at once.
 */
class FoldPostureTest {
    @Test
    fun `no hinges is flat`() {
        assertEquals(FoldPosture.Flat, foldPostureOf(Posture()))
    }

    @Test
    fun `a half-opened horizontal hinge is tabletop`() {
        val bounds = Rect(0f, 500f, 1000f, 520f)
        val posture = Posture(isTabletop = true, hingeList = listOf(hinge(bounds, vertical = false)))

        assertEquals(FoldPosture.Tabletop(bounds), foldPostureOf(posture))
    }

    @Test
    fun `a separating vertical hinge is book`() {
        val bounds = Rect(600f, 0f, 620f, 1000f)
        val posture = Posture(hingeList = listOf(hinge(bounds, vertical = true)))

        assertEquals(FoldPosture.Book(bounds), foldPostureOf(posture))
    }

    @Test
    fun `a flat vertical hinge is not book`() {
        // A fully-opened foldable is one continuous surface. Splitting the layout at a crease
        // the user is looking straight through would be a worse layout, not a smarter one.
        val bounds = Rect(600f, 0f, 620f, 1000f)
        val posture = Posture(hingeList = listOf(hinge(bounds, vertical = true, flat = true)))

        assertEquals(FoldPosture.Flat, foldPostureOf(posture))
    }

    @Test
    fun `a non-separating vertical hinge is not book`() {
        val bounds = Rect(600f, 0f, 620f, 1000f)
        val posture = Posture(hingeList = listOf(hinge(bounds, vertical = true, separating = false)))

        assertEquals(FoldPosture.Flat, foldPostureOf(posture))
    }

    @Test
    fun `tabletop wins over book on a dual-hinge device`() {
        val horizontal = Rect(0f, 500f, 1000f, 520f)
        val vertical = Rect(600f, 0f, 620f, 1000f)
        val posture =
            Posture(
                isTabletop = true,
                hingeList = listOf(hinge(vertical, vertical = true), hinge(horizontal, vertical = false)),
            )

        assertEquals(FoldPosture.Tabletop(horizontal), foldPostureOf(posture))
    }

    @Test
    fun `the hinge to avoid is exposed for both folded postures`() {
        val bounds = Rect(600f, 0f, 620f, 1000f)

        assertEquals(bounds, FoldPosture.Book(bounds).hingeBounds)
        assertEquals(bounds, FoldPosture.Tabletop(bounds).hingeBounds)
        assertEquals(null, FoldPosture.Flat.hingeBounds)
    }

    private fun hinge(
        bounds: Rect,
        vertical: Boolean,
        flat: Boolean = false,
        separating: Boolean = true,
    ) = HingeInfo(
        bounds = bounds,
        isFlat = flat,
        isVertical = vertical,
        isSeparating = separating,
        isOccluding = false,
    )
}
