package com.justin13888.capsule.feature.sharing

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.isToggleable
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.sharing.ScreenHarness.setThemedContent
import com.justin13888.capsule.feature.sharing.ScreenHarness.string
import com.justin13888.capsule.feature.sharing.ui.SHARE_CREATE_DISCLOSURE_TAG
import com.justin13888.capsule.feature.sharing.ui.SHARE_CREATE_SUBMIT_TAG
import com.justin13888.capsule.feature.sharing.ui.ShareLinkCreateActions
import com.justin13888.capsule.feature.sharing.ui.ShareLinkCreateScreen
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The create form, and above all the disclosure on it.
 *
 * The privacy strip is mandatory and has no opt-out, so the test that matters most here is
 * the negative one: there is no toggle anywhere on this screen. A future refactor that
 * "helpfully" turns the disclosure into a switch fails this file before it reaches a review.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ShareLinkCreateScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val events = mutableListOf<String>()

    private val actions =
        ShareLinkCreateActions(
            selectScope = { events += "scope:" + it.name },
            selectAlbum = { events += "album:" + it.value },
            setPassphrase = { events += "passphrase:" + it },
            selectExpiry = { events += "expiry:" + it.name },
            submit = { events += "submit" },
            retry = { events += "retry" },
            back = { events += "back" },
        )

    private fun state(
        assetId: AssetId? = null,
        albums: List<AlbumOption> = listOf(AlbumOption(AlbumId("album-1"), "Iceland")),
    ) = ShareLinkCreateUiState(
        isLoading = false,
        assetId = assetId,
        albums = albums,
        selectedAlbumId = albums.firstOrNull()?.id,
    )

    @Test
    fun `the disclosure lists every field the strip removes`() {
        compose.setThemedContent { ShareLinkCreateScreen(state = state(), actions = actions) }
        compose.onNodeWithTag(SHARE_CREATE_DISCLOSURE_TAG).assertIsDisplayed()
        val body =
            listOf(
                R.string.android_sharing_privacy_camera_serial,
                R.string.android_sharing_privacy_device_ids,
                R.string.android_sharing_privacy_location,
                R.string.android_sharing_privacy_contact_tags,
            ).map(::string)
        val rendered = string(R.string.android_sharing_privacy_body)
        for (field in body + rendered) {
            compose.onNodeWithText(field, substring = true).assertIsDisplayed()
        }
    }

    @Test
    fun `nothing on this screen can switch the privacy strip off`() {
        compose.setThemedContent { ShareLinkCreateScreen(state = state(), actions = actions) }
        val toggles = compose.onAllNodes(isToggleable()).fetchSemanticsNodes()
        assertTrue("the strip has no opt-out, so the form must offer no toggle", toggles.isEmpty())
    }

    @Test
    fun `the single-item scope is offered as unavailable rather than hidden`() {
        compose.setThemedContent { ShareLinkCreateScreen(state = state(), actions = actions) }
        compose.onNodeWithText(string(R.string.android_sharing_scope_asset_unavailable)).assertIsDisplayed()
    }

    @Test
    fun `opening from an item selects the single-item scope`() {
        compose.setThemedContent {
            ShareLinkCreateScreen(
                state = state(assetId = AssetId("asset-1")).copy(scope = ShareScopeChoice.SingleAsset),
                actions = actions,
            )
        }
        compose.onNodeWithText(string(R.string.android_sharing_scope_asset)).assertIsSelected()
    }

    @Test
    fun `picking an expiry reports the preset that was picked`() {
        compose.setThemedContent { ShareLinkCreateScreen(state = state(), actions = actions) }
        compose.onNodeWithText(string(R.string.android_sharing_expiry_one_week)).performScrollTo().performClick()
        assertEquals(listOf("expiry:OneWeek"), events)
    }

    @Test
    fun `a form that describes no scope cannot be submitted`() {
        compose.setThemedContent {
            ShareLinkCreateScreen(state = state(albums = emptyList()), actions = actions)
        }
        compose.onNodeWithTag(SHARE_CREATE_SUBMIT_TAG).performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun `submitting reports once`() {
        compose.setThemedContent { ShareLinkCreateScreen(state = state(), actions = actions) }
        compose.onNodeWithTag(SHARE_CREATE_SUBMIT_TAG).performScrollTo().performClick()
        assertEquals(listOf("submit"), events)
    }

    @Test
    fun `a refusal is explained on the form rather than replacing it`() {
        compose.setThemedContent {
            ShareLinkCreateScreen(
                state = state().copy(failure = SharingFailure(SharingFailureKind.Offline)),
                actions = actions,
            )
        }
        compose.onNodeWithText(string(R.string.android_sharing_offline_title)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(SHARE_CREATE_DISCLOSURE_TAG).assertIsDisplayed()
    }
}
