package com.justin13888.capsule.core.ui.gallery

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The claim zoom makes is narrow and absolute: the photograph under your fingers does not
 * move. Everything else about the gesture — the ladder, the snapping, the double tap — is
 * in service of that, so these check the ladder arithmetic and then check that a real
 * re-plan through a real composition keeps the anchored asset where it was.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w800dp-h1200dp-mdpi")
class GalleryZoomTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `the compact ladder runs seven to one and the wide one adds two coarser rungs`() {
        assertEquals(listOf(7, 5, 3, 2, 1), GalleryDensityLevels.Compact.levels.map { it.assetsAcross })
        assertEquals(listOf(12, 9, 7, 5, 3, 2, 1), GalleryDensityLevels.Expanded.levels.map { it.assetsAcross })
        assertEquals(GalleryDensityLevels.Compact, GalleryDensityLevels.forWidth(411.dp))
        assertEquals(GalleryDensityLevels.Expanded, GalleryDensityLevels.forWidth(900.dp))
    }

    @Test
    fun `a level's target row height leaves room for the gaps between assets`() {
        val level = GalleryDensityLevel(assetsAcross = 4)
        // Four assets and three 4px gaps across 412px leaves 100px each.
        assertEquals(100f, level.targetRowHeight(viewportWidth = 412f, spacing = 4f), 0.001f)
    }

    @Test
    fun `snapping picks the nearest rung in log space`() {
        val ladder = GalleryDensityLevels.Compact
        assertEquals(2, ladder.nearestTo(3f))
        assertEquals(0, ladder.nearestTo(20f))
        assertEquals(4, ladder.nearestTo(0.2f))
        // 4 sits between 3 and 5; log space calls it 3, because 3->4 is the smaller ratio.
        assertEquals(2, ladder.nearestTo(3.8f))
    }

    @Test
    fun `double tap returns to the rung it came from`() {
        val zoom = GalleryZoomState(GalleryDensityLevels.Compact, initialIndex = 2, initialPreviousIndex = 2)
        zoom.zoomIn()
        assertEquals(3, zoom.levelIndex)
        zoom.toggle()
        assertEquals(2, zoom.levelIndex)
        zoom.toggle()
        assertEquals(3, zoom.levelIndex)
    }

    @Test
    fun `a re-plan at a new density leaves the anchored asset where it was`() {
        val state = GalleryState()
        val zoom = GalleryZoomState(GalleryDensityLevels.Compact, initialIndex = 2, initialPreviousIndex = 2)
        compose.setContent { ZoomHarness(state = state, zoom = zoom) }
        compose.waitForIdle()

        compose.runOnIdle { state.dispatchRawDelta(6_000f) }
        compose.waitForIdle()
        val firstPlan = requireNotNull(state.plan)
        assertTrue("the fixture must actually be scrolled", state.scrollY > 0f)

        // What the pinch release does, in the order it does it.
        val anchor = compose.runOnIdle { requireNotNull(state.captureAnchor()) }
        val expected = anchor.viewportFraction * state.viewportHeight
        compose.runOnIdle { zoom.zoomIn() }
        compose.waitForIdle()

        val secondPlan = requireNotNull(state.plan)
        assertNotEquals("the density change must have produced a new plan", firstPlan, secondPlan)
        val actual = secondPlan.offsetOf(secondPlan.slotOfAsset(anchor.asset)) - state.scrollY
        assertEquals("the anchored asset must stay put across the re-plan", expected, actual, 1f)
    }

    /** A gallery whose plan is re-solved whenever the zoom level changes, as a feature would. */
    @androidx.compose.runtime.Composable
    private fun ZoomHarness(
        state: GalleryState,
        zoom: GalleryZoomState,
    ) {
        CapsuleTheme {
            val plan =
                remember(zoom.level) {
                    val target = zoom.level.targetRowHeight(GalleryFixture.VIEWPORT_WIDTH, spacing = 4f)
                    GalleryFixture.plan(metrics = GalleryFixture.Metrics.copy(targetRowHeight = target))
                }
            Box(modifier = Modifier.size(GalleryFixture.VIEWPORT_WIDTH.dp, GalleryFixture.VIEWPORT_HEIGHT.dp)) {
                JustifiedGallery(
                    plan = plan,
                    state = state,
                    zoom = zoom,
                    header = { Box(Modifier.fillMaxSize()) },
                    cell = { asset, _, _ -> Box(Modifier.fillMaxSize().testTag("cell-$asset")) },
                )
            }
        }
    }
}
