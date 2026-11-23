package com.justin13888.capsule.feature.viewer

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import com.justin13888.capsule.feature.viewer.media.ZoomPan
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The one thing zoom and pan promise: the photograph never leaves the frame.
 *
 * Every case here is a clamp. The arithmetic is easy to write and easy to get subtly wrong —
 * an overhang computed against the full frame instead of half of it, a translation applied
 * before its scale — and every one of those mistakes looks fine in a screenshot and wrong in
 * the hand, which is exactly why they are asserted as numbers rather than by gesture.
 */
class ZoomPanTest {
    private val frame = Size(width = 400f, height = 800f)

    @Test
    fun `the fitted position is centred at unit scale`() {
        assertEquals(ZoomPan.MIN_SCALE, ZoomPan.Fitted.scale)
        assertEquals(Offset.Zero, ZoomPan.Fitted.offset)
        assertTrue(ZoomPan.Fitted.isFitted)
    }

    @Test
    fun `pinching in stops at the maximum scale`() {
        val zoomed = ZoomPan.Fitted.transformed(zoomChange = 100f, panChange = Offset.Zero, frame = frame)

        assertEquals(ZoomPan.MAX_SCALE, zoomed.scale)
    }

    @Test
    fun `pinching out stops at the fit and recentres`() {
        val zoomed = ZoomPan(scale = 3f, offset = Offset(50f, 50f))

        val fitted = zoomed.transformed(zoomChange = 0.01f, panChange = Offset.Zero, frame = frame)

        assertEquals(ZoomPan.MIN_SCALE, fitted.scale)
        // Below the fit there is no overhang, so the only legal position is the centre.
        assertEquals(Offset.Zero, fitted.offset)
    }

    @Test
    fun `panning is clamped to the overhang the scale allows`() {
        val zoomed = ZoomPan(scale = 2f)

        val dragged = zoomed.transformed(zoomChange = 1f, panChange = Offset(10_000f, 10_000f), frame = frame)

        // At scale 2 the photo overhangs by half the frame on each side.
        assertEquals(frame.width / 2f, dragged.offset.x)
        assertEquals(frame.height / 2f, dragged.offset.y)
    }

    @Test
    fun `panning at the fit cannot move the photo at all`() {
        val dragged = ZoomPan.Fitted.transformed(zoomChange = 1f, panChange = Offset(300f, 300f), frame = frame)

        assertEquals(Offset.Zero, dragged.offset)
    }

    @Test
    fun `a double tap from the fit zooms in about the tapped point`() {
        // A tap on the left edge, halfway down: 200dp left of centre, level with it.
        val toggled = ZoomPan.Fitted.toggled(tap = Offset(0f, frame.height / 2f), frame = frame)

        assertEquals(ZoomPan.DOUBLE_TAP_SCALE, toggled.scale)
        assertTrue(toggled.offset.x > 0f) { "the tapped point should be pulled toward the centre" }
        // A tap level with the centre must not move the photo vertically at all. Compared
        // with a tolerance because negating a zero produces `-0.0f`, which is a different
        // float from `0.0f` and an identical pixel.
        assertEquals(0f, toggled.offset.y, TOLERANCE)
    }

    @Test
    fun `a double tap while zoomed returns to the fit`() {
        val zoomed = ZoomPan(scale = 4f, offset = Offset(100f, 100f))

        val toggled = zoomed.toggled(tap = Offset(10f, 10f), frame = frame)

        assertEquals(ZoomPan.Fitted, toggled)
    }

    @Test
    fun `a double tap never strands the photo outside the frame`() {
        val toggled = ZoomPan.Fitted.toggled(tap = Offset.Zero, frame = frame)

        val maxX = frame.width * (ZoomPan.DOUBLE_TAP_SCALE - 1f) / 2f
        assertTrue(toggled.offset.x <= maxX) { "offset ${toggled.offset.x} exceeds the overhang $maxX" }
    }

    @Test
    fun `stepping out from the fit stays fitted`() {
        val stepped = ZoomPan.Fitted.stepped(1f / ZoomPan.STEP_FACTOR, frame)

        assertEquals(ZoomPan.Fitted, stepped)
    }

    @Test
    fun `stepping in re-clamps a translation the new scale no longer allows`() {
        val zoomed = ZoomPan(scale = 3f, offset = Offset(x = frame.width, y = 0f))
        val pinned = zoomed.stepped(1f, frame)

        val stepped = pinned.stepped(1f / ZoomPan.STEP_FACTOR, frame)

        val maxX = frame.width * (stepped.scale - 1f) / 2f
        assertTrue(stepped.offset.x <= maxX + TOLERANCE) { "offset ${stepped.offset.x} exceeds $maxX" }
    }

    private companion object {
        const val TOLERANCE = 0.001f
    }
}
