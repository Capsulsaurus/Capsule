package com.justin13888.capsule.core.ui.gallery

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The scrubber's whole value is that dragging it lands where it says it will. A rail that
 * is approximately right is a rail nobody uses twice, so the mapping from a touch on the
 * track to a scroll offset is pinned rather than eyeballed.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w800dp-h1200dp-mdpi")
class GalleryScrubberTest {
    @get:Rule
    val compose = createComposeRule()

    private val plan = GalleryFixture.plan()

    @Test
    fun `dragging the rail to its midpoint scrolls to the middle of the library`() {
        val state = GalleryState()
        compose.setContent { ScrubberHarness(plan = plan, state = state) }
        compose.waitForIdle()

        val rail = compose.onNodeWithTag(GalleryTestTags.SCRUBBER)
        val height =
            rail
                .fetchSemanticsNode()
                .size.height
                .toFloat()
        rail.performTouchInput {
            down(Offset(centerX, TOP_OF_TRACK))
            moveTo(Offset(centerX, height / 2f))
            up()
        }
        compose.waitForIdle()

        assertEquals("a midpoint drag should land mid-library", 0.5f, state.scrollFraction, 0.06f)
    }

    @Test
    fun `dragging to the bottom of the rail reaches the end of the library`() {
        val state = GalleryState()
        compose.setContent { ScrubberHarness(plan = plan, state = state) }
        compose.waitForIdle()

        val rail = compose.onNodeWithTag(GalleryTestTags.SCRUBBER)
        val height =
            rail
                .fetchSemanticsNode()
                .size.height
                .toFloat()
        rail.performTouchInput {
            down(Offset(centerX, TOP_OF_TRACK))
            moveTo(Offset(centerX, height))
            up()
        }
        compose.waitForIdle()

        assertEquals(1f, state.scrollFraction, 0.001f)
        assertEquals(GalleryScroll.maxScroll(plan, state.viewportHeight), state.scrollY, 1f)
    }

    @Test
    fun `the rail is labelled and offers section jumps to a screen reader`() {
        val state = GalleryState()
        compose.setContent { ScrubberHarness(plan = plan, state = state) }
        compose.waitForIdle()

        val config = compose.onNodeWithTag(GalleryTestTags.SCRUBBER).fetchSemanticsNode().config
        assertNotNull("the rail must be labelled", config.getOrNull(SemanticsProperties.ContentDescription))
        val actions = config.getOrNull(SemanticsActions.CustomActions)
        assertNotNull("the rail must be reachable without a drag", actions)
        assertEquals(2, actions?.size)
    }

    @Test
    fun `the date bubble appears while the rail is being dragged`() {
        val state = GalleryState()
        compose.setContent { ScrubberHarness(plan = plan, state = state) }
        compose.waitForIdle()

        compose.onNodeWithTag(GalleryTestTags.SCRUBBER_BUBBLE).assertDoesNotExist()

        val rail = compose.onNodeWithTag(GalleryTestTags.SCRUBBER)
        val height =
            rail
                .fetchSemanticsNode()
                .size.height
                .toFloat()
        rail.performTouchInput {
            down(Offset(centerX, TOP_OF_TRACK))
            moveTo(Offset(centerX, height / 3f))
        }
        compose.waitForIdle()
        compose.onNodeWithTag(GalleryTestTags.SCRUBBER_BUBBLE).assertExists()
        assertTrue(state.scrollY > 0f)

        rail.performTouchInput { up() }
        compose.waitForIdle()
        compose.onNodeWithTag(GalleryTestTags.SCRUBBER_BUBBLE).assertDoesNotExist()
    }

    /** A gallery and its rail, sized so the track height is a number the test can name. */
    @Composable
    private fun ScrubberHarness(
        plan: LayoutPlan,
        state: GalleryState,
    ) {
        CapsuleTheme {
            Box(modifier = Modifier.size(GalleryFixture.VIEWPORT_WIDTH.dp, GalleryFixture.VIEWPORT_HEIGHT.dp)) {
                GalleryHarness(plan = plan, state = state)
                GalleryScrubber(
                    plan = plan,
                    state = state,
                    sectionLabel = { "section-$it" },
                    modifier = Modifier.align(Alignment.CenterEnd),
                    autoHide = false,
                )
            }
        }
    }

    private companion object {
        /** Just inside the top of the rail, past the thumb's half-height offset. */
        const val TOP_OF_TRACK = 2f
    }
}
