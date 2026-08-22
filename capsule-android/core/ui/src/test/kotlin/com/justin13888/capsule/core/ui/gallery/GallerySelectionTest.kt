package com.justin13888.capsule.core.ui.gallery

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performTouchInput
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Selection is the single most common thing a photographer does here, and a sweep that
 * selects the wrong range is worse than one that selects nothing — it is destructive at the
 * next tap. These pin the range down at both levels: the set semantics on their own, and
 * the whole gesture driven through real pointer events against the plan's hit test.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w800dp-h1200dp-mdpi")
class GallerySelectionTest {
    @get:Rule
    val compose = createComposeRule()

    private val plan = GalleryFixture.plan()

    @Test
    fun `a drag selects the inclusive range between the anchor and the finger`() {
        val selection = SelectionState()
        selection.beginDrag(4)
        selection.dragTo(9)
        assertArrayEquals(intArrayOf(4, 5, 6, 7, 8, 9), selection.snapshot())
    }

    @Test
    fun `dragging backwards past the anchor selects the other way`() {
        val selection = SelectionState()
        selection.beginDrag(9)
        selection.dragTo(6)
        assertArrayEquals(intArrayOf(6, 7, 8, 9), selection.snapshot())
    }

    @Test
    fun `dragging back the way you came releases what it selected`() {
        val selection = SelectionState()
        selection.beginDrag(4)
        selection.dragTo(12)
        selection.dragTo(6)
        assertArrayEquals(intArrayOf(4, 5, 6), selection.snapshot())
    }

    @Test
    fun `a drag extends the selection it started from rather than replacing it`() {
        val selection = SelectionState()
        selection.toggle(100)
        selection.beginDrag(2)
        selection.dragTo(4)
        assertArrayEquals(intArrayOf(2, 3, 4, 100), selection.snapshot())
    }

    @Test
    fun `clearing leaves selection mode`() {
        val selection = SelectionState()
        selection.toggle(1)
        assertTrue(selection.isActive)
        selection.clear()
        assertFalse(selection.isActive)
        assertArrayEquals(IntArray(0), selection.snapshot())
    }

    @Test
    fun `a long press and sweep selects every asset between the two touch points`() {
        val selection = SelectionState()
        val state = GalleryState()
        compose.setContent { GalleryHarness(plan = plan, state = state, selection = selection) }
        compose.waitForIdle()

        // Sample two real points out of the plan rather than guessing at pixels: the first
        // row starts under the header, and the row heights are whatever the solver chose.
        val start = pointOnAsset(asset = plan.rowFrom(1))
        val endAsset = plan.rowUntil(1) - 1
        val end = pointOnAsset(asset = endAsset)

        compose.onNodeWithTag(GalleryTestTags.GRID).performTouchInput {
            down(start)
            advanceEventTime(LONG_PRESS_MILLIS)
            moveTo(end)
            advanceEventTime(FRAME_MILLIS)
            up()
        }
        compose.waitForIdle()

        assertTrue("a long press must enter selection mode", selection.isActive)
        assertArrayEquals(
            "the sweep should own the whole first row",
            IntArray(endAsset - plan.rowFrom(1) + 1) { plan.rowFrom(1) + it },
            selection.snapshot(),
        )
    }

    /** The centre of [asset] in viewport coordinates, with the gallery unscrolled. */
    private fun pointOnAsset(asset: Int): Offset {
        val slot = plan.slotOfAsset(asset)
        val x = plan.assetX(slot, asset) + plan.assetWidth(slot, asset) / 2f
        val y = plan.offsetOf(slot) + plan.heightOf(slot) / 2f
        return Offset(x, y)
    }

    private companion object {
        const val LONG_PRESS_MILLIS = 1_000L
        const val FRAME_MILLIS = 16L
    }
}
