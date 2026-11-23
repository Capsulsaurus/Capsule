package com.justin13888.capsule.feature.timeline.cull

import android.content.Context
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.ui.gallery.GalleryCellMarkers
import com.justin13888.capsule.feature.timeline.TimelineCell
import com.justin13888.capsule.feature.timeline.TimelineTestTags
import com.justin13888.capsule.feature.timeline.launch
import com.justin13888.capsule.feature.timeline.withArguments
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Instant

/**
 * The review surface: the one-up, the filmstrip, the flags, and the one destructive batch.
 *
 * The state machine is pinned down separately and purely; what these assert is that the screen
 * offers every decision the pass needs, and that the sweep cannot fire without being asked for
 * twice.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class CullingReviewTest {
    @get:Rule
    val compose = createComposeRule()

    private val context: Context get() = ApplicationProvider.getApplicationContext()

    private fun string(id: Int): String = context.getString(id)

    private fun frame(
        asset: Int,
        flag: CullFlag?,
        rating: Int = 3,
    ) = CullFrame(
        asset = asset,
        cell =
            TimelineCell(
                assetId = AssetId("asset-$asset"),
                markers = GalleryCellMarkers.None,
                degradation = null,
                placeholder = Color.DarkGray,
                capturedAt = Instant.EPOCH,
                isVideo = false,
                cull = flag,
                rating = rating,
            ),
        flag = flag,
    )

    private val session =
        CullingUiState.over(
            frames =
                listOf(
                    frame(0, CullFlag.Neutral),
                    frame(1, CullFlag.Reject),
                    frame(2, CullFlag.Pick),
                ),
            origin = 0,
        )

    @Test
    fun `the review shows the frame under review, the filmstrip, and where it is in the set`() {
        compose.launch { CullingReview(state = session, callbacks = CullingCallbacks()) }

        compose.onNodeWithTag(TimelineTestTags.REVIEW).assertExists()
        compose.onNodeWithTag(TimelineTestTags.REVIEW_FRAME).assertExists()
        compose.onNodeWithTag(TimelineTestTags.REVIEW_FILMSTRIP).assertExists()
        val position = string(R.string.timeline_cull_position).withArguments("position" to 1, "total" to 3)
        compose.onNodeWithText(position).assertIsDisplayed()
    }

    @Test
    fun `all three flags are offered, because undecided is a real decision to return to`() {
        val flagged = mutableListOf<CullFlag>()
        compose.launch {
            CullingReview(state = session, callbacks = CullingCallbacks(onFlag = flagged::add))
        }

        compose.onNodeWithText(string(R.string.timeline_cull_pick)).performClick()
        compose.onNodeWithText(string(R.string.timeline_cull_reject)).performClick()
        compose.onNodeWithText(string(R.string.timeline_cull_neutral)).performClick()

        assertEquals(listOf(CullFlag.Pick, CullFlag.Reject, CullFlag.Neutral), flagged)
    }

    @Test
    fun `the set can be walked without a swipe or a keyboard`() {
        val moved = mutableListOf<Int>()
        compose.launch {
            CullingReview(state = session, callbacks = CullingCallbacks(onMove = moved::add))
        }

        compose.onNodeWithContentDescription(string(R.string.timeline_cull_a11y_next)).performClick()
        compose.onNodeWithContentDescription(string(R.string.timeline_cull_a11y_previous)).performClick()

        assertEquals(listOf(1, -1), moved)
    }

    @Test
    fun `the rating is shown beside the flag, because the two are independent`() {
        compose.launch { CullingReview(state = session, callbacks = CullingCallbacks()) }

        compose
            .onNodeWithText(string(R.string.timeline_cull_rating).withArguments("rating" to 3))
            .assertIsDisplayed()
    }

    @Test
    fun `the filter chips carry the counts the sweep will act on`() {
        val filters = mutableListOf<CullFilter>()
        compose.launch {
            CullingReview(state = session, callbacks = CullingCallbacks(onFilter = filters::add))
        }

        compose.onNodeWithText("${string(R.string.timeline_cull_filter_rejects)} (1)").performClick()

        assertEquals(listOf(CullFilter.Rejects), filters)
    }

    @Test
    fun `a filter that matches nothing says so instead of showing a blank frame`() {
        val nothingPicked =
            CullingUiState
                .over(listOf(frame(0, CullFlag.Reject)), origin = 0)
                .withFilter(CullFilter.Picks)
        compose.launch { CullingReview(state = nothingPicked, callbacks = CullingCallbacks()) }

        compose.onNodeWithText(string(R.string.timeline_cull_empty_title)).assertIsDisplayed()
        compose.onNodeWithTag(TimelineTestTags.REVIEW_FRAME).assertDoesNotExist()
    }

    @Test
    fun `the sweep names its count and asks before it moves anything`() {
        var confirming = false
        var swept = 0
        compose.launch {
            CullingReview(
                state = session.confirmingSweep(confirming),
                callbacks = CullingCallbacks(onConfirmSweep = { confirming = it }, onSweep = { swept++ }),
            )
        }

        val sweep = string(R.string.timeline_cull_sweep).withArguments("count" to 1)
        compose.onNodeWithText(sweep).assertIsEnabled().performClick()

        assertEquals(true, confirming)
        assertEquals(0, swept)
    }

    @Test
    fun `the confirmation states that the sweep is reversible, then runs it`() {
        var swept = 0
        compose.launch {
            CullingReview(
                state = session.confirmingSweep(true),
                callbacks = CullingCallbacks(onSweep = { swept++ }),
            )
        }

        compose.onNodeWithTag(TimelineTestTags.REVIEW_SWEEP_CONFIRM).assertExists()
        val body = string(R.string.timeline_cull_sweep_confirm_body).withArguments("count" to 1)
        compose.onNodeWithText(body).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.timeline_cull_sweep_confirm_action)).performClick()

        assertEquals(1, swept)
    }

    @Test
    fun `a review with nothing rejected cannot start a sweep at all`() {
        val nothingRejected = CullingUiState.over(listOf(frame(0, CullFlag.Pick)), origin = 0)
        compose.launch { CullingReview(state = nothingRejected, callbacks = CullingCallbacks()) }

        val sweep = string(R.string.timeline_cull_sweep).withArguments("count" to 0)
        compose.onNodeWithText(sweep).assertIsNotEnabled()
    }

    @Test
    fun `leaving the review is always one press away`() {
        var closed = 0
        compose.launch {
            CullingReview(state = session, callbacks = CullingCallbacks(onClose = { closed++ }))
        }

        compose.onNodeWithContentDescription(string(R.string.timeline_cull_exit)).performClick()

        assertEquals(1, closed)
    }
}
