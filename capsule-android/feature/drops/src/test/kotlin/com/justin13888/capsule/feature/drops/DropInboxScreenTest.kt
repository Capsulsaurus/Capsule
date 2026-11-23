package com.justin13888.capsule.feature.drops

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.feature.drops.DropsScreenHarness.setThemedContent
import com.justin13888.capsule.feature.drops.DropsScreenHarness.string
import com.justin13888.capsule.feature.drops.ui.DROP_ADOPT_TAG
import com.justin13888.capsule.feature.drops.ui.DROP_ALBUM_PICKER_TAG
import com.justin13888.capsule.feature.drops.ui.DROP_DISCARD_CONFIRM_TAG
import com.justin13888.capsule.feature.drops.ui.DROP_DISCARD_TAG
import com.justin13888.capsule.feature.drops.ui.DROP_REVIEW_BANNER_TAG
import com.justin13888.capsule.feature.drops.ui.DROP_ROW_TAG
import com.justin13888.capsule.feature.drops.ui.DropInboxActions
import com.justin13888.capsule.feature.drops.ui.DropInboxScreen
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The inbox as a review surface.
 *
 * The three assertions that carry the design are here: the screen says up front that nothing
 * has been applied, the guest's filename is labelled as the guest's unverified claim, and
 * adoption goes through a picker that quotes the origin note before anything moves.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class DropInboxScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val events = mutableListOf<String>()

    private val album = AlbumOption(AlbumId("album-1"), "Iceland")

    private val actions =
        DropInboxActions(
            beginAdoption = { events += "begin:" + it.value },
            cancelAdoption = { events += "cancel" },
            adopt = { drop, into -> events += "adopt:" + drop.value + ":" + into.value },
            discard = { events += "discard:" + it.value },
            retry = { events += "retry" },
            back = { events += "back" },
        )

    private fun state(
        drops: List<DropRow> = listOf(DropRow(DropsScreenHarness.drop(), viaLinkOpaqueId = "3c59dc04")),
        albums: List<AlbumOption> = listOf(album),
        choosingAlbumFor: com.justin13888.capsule.core.model.DropId? = null,
    ) = DropInboxUiState(
        isLoading = false,
        drops = drops,
        albums = albums,
        choosingAlbumFor = choosingAlbumFor,
    )

    private fun render(state: DropInboxUiState) {
        compose.setThemedContent { DropInboxScreen(state = state, actions = actions) }
    }

    @Test
    fun `the loading state names what it is waiting for`() {
        render(DropInboxUiState())
        compose.onNodeWithText(string(R.string.android_drops_inbox_loading)).assertIsDisplayed()
    }

    @Test
    fun `an empty inbox explains where files would come from`() {
        render(DropInboxUiState(isLoading = false))
        compose.onNodeWithText(string(R.string.android_drops_inbox_empty_title)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_drops_inbox_empty_body)).assertIsDisplayed()
    }

    @Test
    fun `the inbox states that nothing in it has been applied`() {
        render(state())
        compose.onNodeWithTag(DROP_REVIEW_BANNER_TAG).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_drops_review_banner_title)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_drops_review_banner_body)).assertIsDisplayed()
    }

    @Test
    fun `the sender's filename is shown as the sender's unverified claim`() {
        render(state())
        compose.onNodeWithText("wedding-ceremony-0431.jpg").assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_drops_name_unverified)).assertIsDisplayed()
    }

    @Test
    fun `a deposit with no name is labelled rather than left blank`() {
        render(state(drops = listOf(DropRow(DropsScreenHarness.drop(suggestedFilename = null)))))
        compose.onNodeWithText(string(R.string.android_drops_unnamed)).assertIsDisplayed()
    }

    @Test
    fun `adopting opens a picker rather than moving anything`() {
        render(state())
        compose.onNodeWithTag(DROP_ADOPT_TAG + "drop-under-test").performClick()
        assertEquals(listOf("begin:drop-under-test"), events)
    }

    @Test
    fun `the picker quotes the unverified guest-origin note before an album is chosen`() {
        render(
            state(
                choosingAlbumFor =
                    com.justin13888.capsule.core.model
                        .DropId("drop-under-test"),
            ),
        )
        compose.onNodeWithTag(DROP_ALBUM_PICKER_TAG).assertIsDisplayed()
        val note = string(R.string.android_drops_origin_note).substringBefore("{link}")
        compose.onNodeWithText(note, substring = true).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_drops_adopt_body)).assertIsDisplayed()
        assertEquals(emptyList<String>(), events)
    }

    @Test
    fun `choosing an album is what finally adopts`() {
        render(
            state(
                choosingAlbumFor =
                    com.justin13888.capsule.core.model
                        .DropId("drop-under-test"),
            ),
        )
        compose.onNodeWithText("Iceland").performClick()
        assertEquals(listOf("adopt:drop-under-test:album-1"), events)
    }

    @Test
    fun `discarding asks first and only then reports`() {
        render(state())
        compose.onNodeWithTag(DROP_DISCARD_TAG + "drop-under-test").performClick()
        compose.onNodeWithText(string(R.string.android_drops_discard_confirm_title)).assertIsDisplayed()
        assertEquals(emptyList<String>(), events)
        compose.onNodeWithTag(DROP_DISCARD_CONFIRM_TAG).performClick()
        assertEquals(listOf("discard:drop-under-test"), events)
    }

    @Test
    fun `with no album to adopt into the action is refused and explained`() {
        render(state(albums = emptyList()))
        compose.onNodeWithTag(DROP_ADOPT_TAG + "drop-under-test").assertIsNotEnabled()
        compose.onNodeWithText(string(R.string.android_drops_adopt_no_albums)).assertIsDisplayed()
    }

    @Test
    fun `a refused operation is explained above the deposits, which stay listed`() {
        render(state().copy(failure = DropsFailure(DropsFailureKind.Offline)))
        compose.onNodeWithText(string(R.string.android_drops_offline_title)).assertIsDisplayed()
        compose.onNodeWithTag(DROP_ROW_TAG + "drop-under-test").assertIsDisplayed()
    }

    @Test
    fun `a failure with nothing to show offers a retry`() {
        render(DropInboxUiState(isLoading = false, failure = DropsFailure(DropsFailureKind.Unknown)))
        compose.onNodeWithText(string(R.string.android_drops_retry)).performClick()
        assertEquals(listOf("retry"), events)
    }
}
