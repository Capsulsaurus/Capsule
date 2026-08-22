package com.justin13888.capsule.core.ui.gallery

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onParent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * What the rendering layer promises on top of the solved plan.
 *
 * The layout maths is already pinned down by `LayoutPlannerTest` and `GalleryScrollTest`;
 * everything here is about the Compose surface honouring it — that a library of six hundred
 * photographs costs a screenful of composition, that scrolling brings the next rows in, that
 * the pinned header names the day you are actually looking at, and that a screen reader can
 * describe a grid whose geometry no accessibility service can see.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w800dp-h1200dp-mdpi")
class JustifiedGalleryTest {
    @get:Rule
    val compose = createComposeRule()

    private val plan = GalleryFixture.plan()

    @Test
    fun `only the visible window is composed for a large library`() {
        val composed = mutableSetOf<Int>()
        val state = GalleryState()
        compose.setContent { GalleryHarness(plan = plan, state = state, onCellComposed = { composed += it }) }
        compose.waitForIdle()

        assertTrue("a 600-asset plan should not compose everything, composed ${composed.size}", composed.size < 200)
        assertTrue("the first asset must be on screen", 0 in composed)
        // The viewport plus overscan reaches nowhere near the end of the library.
        assertTrue("nothing near the end should be composed", composed.none { it > 300 })
    }

    @Test
    fun `scrolling composes the rows that come into view`() {
        val composed = mutableSetOf<Int>()
        val state = GalleryState()
        compose.setContent { GalleryHarness(plan = plan, state = state, onCellComposed = { composed += it }) }
        compose.waitForIdle()
        val before = composed.max()

        compose.runOnIdle { state.dispatchRawDelta(4_000f) }
        compose.waitForIdle()

        assertTrue("the scroll should have moved", state.scrollY > 0f)
        assertTrue("later rows should now be composed, was $before now ${composed.max()}", composed.max() > before)
        val onScreen = plan.assetAt(GalleryFixture.VIEWPORT_WIDTH / 4f, state.scrollY + 200f)
        assertTrue("the asset under the viewport must be composed", onScreen < 0 || onScreen in composed)
    }

    @Test
    fun `the pinned header names the section at the top of the viewport`() {
        val state = GalleryState()
        compose.setContent { GalleryHarness(plan = plan, state = state) }
        compose.waitForIdle()

        compose.onNodeWithTag(GalleryFixture.headerTag(0, pinned = true)).assertIsDisplayed()

        // Land squarely inside a later section rather than on a boundary, so the expected
        // answer is not a question of rounding.
        val target = plan.sectionStart(4)
        compose.runOnIdle { state.dispatchRawDelta(plan.offsetOfAsset(target, GalleryFixture.VIEWPORT_HEIGHT) + 40f) }
        compose.waitForIdle()

        val section = state.firstVisibleSection
        assertEquals("scrolling into section 4 should pin section 4", 4, section)
        compose.onNodeWithTag(GalleryFixture.headerTag(4, pinned = true)).assertIsDisplayed()
    }

    @Test
    fun `the grid and its cells carry the semantics TalkBack navigates by`() {
        val state = GalleryState()
        compose.setContent { GalleryHarness(plan = plan, state = state) }
        compose.waitForIdle()

        val grid = compose.onNodeWithTag(GalleryTestTags.GRID).fetchSemanticsNode()
        val collection = grid.config.getOrNull(SemanticsProperties.CollectionInfo)
        assertNotNull("the grid must expose collectionInfo", collection)
        assertEquals(plan.slotCount, collection?.rowCount)

        val cell = compose.onNodeWithTag("cell-0", useUnmergedTree = true).onParent().fetchSemanticsNode()
        val item = cell.config.getOrNull(SemanticsProperties.CollectionItemInfo)
        assertNotNull("every cell must expose collectionItemInfo", item)
        assertEquals(0, item?.columnIndex)
        assertEquals(
            "the caller's description must reach the semantics tree",
            listOf("asset-0"),
            cell.config.getOrNull(SemanticsProperties.ContentDescription),
        )
    }

    @Test
    fun `an empty section list still renders`() {
        val single = GalleryFixture.plan(assets = 1, perSection = 1)
        val state = GalleryState()
        compose.setContent { GalleryHarness(plan = single, state = state) }
        compose.waitForIdle()

        compose.onNodeWithTag("cell-0", useUnmergedTree = true).assertExists()
        assertEquals(0f, state.scrollY)
    }
}
