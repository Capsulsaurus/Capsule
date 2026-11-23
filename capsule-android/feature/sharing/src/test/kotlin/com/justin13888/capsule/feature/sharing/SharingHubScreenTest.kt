package com.justin13888.capsule.feature.sharing

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.sharing.ScreenHarness.setThemedContent
import com.justin13888.capsule.feature.sharing.ScreenHarness.string
import com.justin13888.capsule.feature.sharing.ui.SHARING_HUB_DROP_INBOX_TAG
import com.justin13888.capsule.feature.sharing.ui.SHARING_HUB_SHARE_LINKS_TAG
import com.justin13888.capsule.feature.sharing.ui.SHARING_HUB_UPLOAD_LINKS_TAG
import com.justin13888.capsule.feature.sharing.ui.SharingHubActions
import com.justin13888.capsule.feature.sharing.ui.SharingHubScreen
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The hub in each of the states it can be in.
 *
 * Every one of them is reachable from a plain value, which is the point of the screen taking
 * state rather than a view model: a loading hub, an offline hub and a hub with three
 * deposits waiting are all one line apart here.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class SharingHubScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private var opened = mutableListOf<String>()

    private val actions =
        SharingHubActions(
            openShareLinks = { opened += "share-links" },
            openUploadLinks = { opened += "upload-links" },
            openDropInbox = { opened += "drop-inbox" },
            createShareLink = { opened += "create-share" },
            createUploadLink = { opened += "create-upload" },
            retry = { opened += "retry" },
        )

    @Test
    fun `the loading state names what it is waiting for`() {
        compose.setThemedContent { SharingHubScreen(state = SharingUiState(), actions = actions) }
        compose.onNodeWithText(string(R.string.android_sharing_loading)).assertIsDisplayed()
    }

    @Test
    fun `an empty hub explains both kinds of link and offers the first one`() {
        compose.setThemedContent {
            SharingHubScreen(state = SharingUiState(isLoading = false), actions = actions)
        }
        compose.onNodeWithText(string(R.string.android_sharing_empty_title)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_destination_share_link_create)).performClick()
        assertEquals(listOf("create-share"), opened)
    }

    @Test
    fun `each row opens its own destination`() {
        compose.setThemedContent {
            SharingHubScreen(
                state = SharingUiState(isLoading = false, uploadLinkCount = 1, pendingDropCount = 3),
                actions = actions,
            )
        }
        compose.onNodeWithTag(SHARING_HUB_SHARE_LINKS_TAG).performClick()
        compose.onNodeWithTag(SHARING_HUB_UPLOAD_LINKS_TAG).performClick()
        compose.onNodeWithTag(SHARING_HUB_DROP_INBOX_TAG).performClick()
        assertEquals(listOf("share-links", "upload-links", "drop-inbox"), opened)
    }

    @Test
    fun `waiting deposits are counted on the inbox row`() {
        compose.setThemedContent {
            SharingHubScreen(
                state = SharingUiState(isLoading = false, uploadLinkCount = 1, pendingDropCount = 3),
                actions = actions,
            )
        }
        val summary = string(R.string.android_sharing_drops_waiting).replace("{count}", "3")
        compose.onNodeWithText(summary, substring = true).assertIsDisplayed()
    }

    @Test
    fun `an empty inbox says so rather than showing a zero`() {
        compose.setThemedContent {
            SharingHubScreen(
                state = SharingUiState(isLoading = false, uploadLinkCount = 1),
                actions = actions,
            )
        }
        compose.onNodeWithText(string(R.string.android_sharing_drops_none)).assertIsDisplayed()
    }

    @Test
    fun `a failure with nothing to show offers a retry`() {
        compose.setThemedContent {
            SharingHubScreen(
                state =
                    SharingUiState(
                        isLoading = false,
                        failure = SharingFailure(SharingFailureKind.Unknown),
                    ),
                actions = actions,
            )
        }
        compose.onNodeWithText(string(R.string.android_sharing_error_title)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_sharing_retry)).performClick()
        assertEquals(listOf("retry"), opened)
    }

    @Test
    fun `offline keeps the links on screen and explains itself as a mode`() {
        compose.setThemedContent {
            SharingHubScreen(
                state =
                    SharingUiState(
                        isLoading = false,
                        failure = SharingFailure(SharingFailureKind.Offline),
                        shareLinks = listOf(ShareLinkRow(ScreenHarness.link())),
                        uploadLinkCount = 1,
                    ),
                actions = actions,
            )
        }
        compose.onNodeWithText(string(R.string.android_sharing_offline_title)).assertIsDisplayed()
        compose.onNodeWithTag(SHARING_HUB_SHARE_LINKS_TAG).assertIsDisplayed()
        assertTrue("nothing was navigated away from", opened.isEmpty())
    }
}
