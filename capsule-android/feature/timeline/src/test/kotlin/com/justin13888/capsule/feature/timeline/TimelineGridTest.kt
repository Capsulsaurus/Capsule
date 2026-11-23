package com.justin13888.capsule.feature.timeline

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import com.justin13888.capsule.core.model.DayBucket
import com.justin13888.capsule.core.ui.gallery.GalleryDefaults
import com.justin13888.capsule.core.ui.gallery.GalleryDensityLevels
import com.justin13888.capsule.core.ui.gallery.GalleryMetrics
import com.justin13888.capsule.core.ui.gallery.GalleryTestTags
import com.justin13888.capsule.core.ui.gallery.LayoutPlanner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.LocalDate
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/**
 * The three things the grid layer owns on top of `core:ui`'s engine.
 *
 * Which window to read, what the scrubber's rail says, and whether a tap on a photograph opens
 * it *without* stealing the gestures the gallery above it needs. That last one is the reason
 * this test exists at all: the tap detector is hand-rolled precisely so that long-press
 * drag-select and pinch-zoom keep working, and a `clickable` that quietly broke both would look
 * identical to a passing screen test.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-mdpi")
class TimelineGridTest {
    @get:Rule
    val compose = createComposeRule()

    private val metrics =
        GalleryMetrics(
            viewportWidth = 400f,
            targetRowHeight = 120f,
            spacing = 2f,
            headerHeight = 44f,
            sectionSpacing = 12f,
        )

    @Test
    fun `the reported window covers the rows the viewport intersects and not the whole library`() {
        val plan = LayoutPlanner.plan(FloatArray(600) { 1.5f }, intArrayOf(0, 200, 400), metrics)

        val opening = visibleAssets(plan, scrollY = 0f, viewportHeight = 800f, overscan = 0f)

        assertEquals(0, opening.first)
        assertTrue("a screenful is not the library, got ${opening.last}", opening.last < 100)
    }

    @Test
    fun `a viewport sitting on a section boundary still names the rows around it`() {
        val plan = LayoutPlanner.plan(FloatArray(600) { 1.5f }, intArrayOf(0, 200, 400), metrics)
        val boundary = plan.offsetOfAsset(200, viewportHeight = 800f)

        val window = visibleAssets(plan, scrollY = boundary, viewportHeight = 800f, overscan = 0f)

        // A header is not content, but skipping it must not blank the rows it introduces.
        assertTrue("the window must not be empty at a day boundary", !window.isEmpty())
        assertTrue(window.last >= 200)
    }

    @Test
    fun `an unmeasured viewport reports nothing rather than guessing`() {
        val plan = LayoutPlanner.plan(FloatArray(10) { 1f }, intArrayOf(0), metrics)

        assertTrue(visibleAssets(plan, 0f, viewportHeight = 0f, overscan = 0f).isEmpty())
    }

    @Test
    fun `the scrubber gets one tick per month, not one per day`() {
        val labels = TestDateLabels
        val days =
            listOf(
                DayBucket(LocalDate.of(2026, 6, 1), 0, 10),
                DayBucket(LocalDate.of(2026, 6, 14), 10, 10),
                DayBucket(LocalDate.of(2026, 5, 30), 20, 10),
                DayBucket(LocalDate.of(2026, 5, 2), 30, 10),
                DayBucket(LocalDate.of(2025, 5, 2), 40, 10),
            )

        val ticks = monthTicks(days, labels)

        assertEquals(listOf(0, 2, 4), ticks.map { it.section })
    }

    @Test
    fun `tapping a photograph opens it`() {
        val mock = TimelineFixture.repositories()
        val state = readyState(mock)
        val opened = mutableListOf<String>()
        compose.launch {
            TimelineScreen(state = state, callbacks = TimelineCallbacks(onOpenAsset = opened::add))
        }

        compose.onNodeWithTag(TimelineTestTags.tile(0), useUnmergedTree = true).performClick()
        compose.waitForIdle()

        assertEquals(
            listOf(
                state.cells
                    .getValue(0)
                    .assetId.value,
            ),
            opened,
        )
    }

    @Test
    fun `long-pressing the grid enters selection mode instead of opening anything`() {
        val mock = TimelineFixture.repositories()
        val opened = mutableListOf<String>()
        compose.launch {
            TimelineScreen(
                state = readyState(mock),
                callbacks = TimelineCallbacks(onOpenAsset = opened::add),
            )
        }

        compose.onNodeWithTag(GalleryTestTags.GRID).performTouchInput { longClick() }
        compose.waitForIdle()

        // The toolbar is the proof that the gallery's own long press survived a tap handler
        // sitting inside every cell.
        compose.onNodeWithTag(TimelineTestTags.SELECTION_TOOLBAR).assertExists()
        assertEquals(emptyList<String>(), opened)
    }

    @Test
    fun `the grid renders inside the plan the library's own aspect ratios solve`() {
        val mock = TimelineFixture.repositories()
        compose.launch { TimelineScreen(state = readyState(mock), callbacks = TimelineCallbacks()) }

        compose.onNodeWithTag(GalleryTestTags.GRID).assertExists()
        compose.onNodeWithTag(GalleryTestTags.SCRUBBER).assertExists()
        compose.onNodeWithTag(GalleryTestTags.STICKY_HEADER).assertExists()
    }

    @Test
    fun `the gallery's own spacing is what the plan is solved against`() {
        // Guards the one arithmetic this file owns: a target row height derived from the wrong
        // spacing produces rows that do not fill the viewport, everywhere, forever.
        val level = GalleryDensityLevels.Compact[2]
        val target = level.targetRowHeight(viewportWidth = 400f, spacing = 2f)

        assertEquals((400f - 2f * 2) / 3f, target, 0.001f)
        assertEquals(2f, GalleryDefaults.Spacing.value, 0.001f)
    }
}

/**
 * Fixed, non-localized labels.
 *
 * The point of these tests is *which* section gets a tick, not how a month reads in Finnish —
 * pinning the formatter keeps the assertion about the grouping rather than about the JDK's
 * locale data.
 */
private val TestDateLabels =
    TimelineDateLabels(
        zone = ZoneOffset.UTC,
        today = LocalDate.of(2026, 6, 15),
        todayLabel = "today",
        yesterdayLabel = "yesterday",
        dayFormatter = DateTimeFormatter.ofPattern("yyyy-MM-dd"),
        monthFormatter = DateTimeFormatter.ofPattern("yyyy-MM"),
        instantFormatter = DateTimeFormatter.ofPattern("yyyy-MM-dd").withZone(ZoneOffset.UTC),
    )
