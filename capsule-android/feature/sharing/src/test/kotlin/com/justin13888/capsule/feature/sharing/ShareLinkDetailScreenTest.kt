package com.justin13888.capsule.feature.sharing

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.sharing.ScreenHarness.setThemedContent
import com.justin13888.capsule.feature.sharing.ScreenHarness.string
import com.justin13888.capsule.feature.sharing.ui.SHARE_DETAIL_COPY_TAG
import com.justin13888.capsule.feature.sharing.ui.SHARE_DETAIL_REVOKE_TAG
import com.justin13888.capsule.feature.sharing.ui.SHARE_DETAIL_URL_TAG
import com.justin13888.capsule.feature.sharing.ui.ShareLinkDetailScreen
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * One share link's detail.
 *
 * The rendered URL is asserted to still carry its fragment. That is the one thing on this
 * screen a user cannot verify by looking — the URL is long, the fragment looks like noise,
 * and a link that lost it fails only in the recipient's hands.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ShareLinkDetailScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val events = mutableListOf<String>()

    private val secret = "c9f0f895fb98ab9159f51fd0297e236d"
    private val url = "https://capsule.test/s/8f14e45fceea167a5a36dedd4bea2543#" + secret

    private fun state(
        url: String? = this.url,
        isRevoked: Boolean = false,
    ) = ShareLinkDetailUiState(
        isLoading = false,
        row = ShareLinkRow(ScreenHarness.link(), albumName = "Iceland"),
        url = url,
        isRevoked = isRevoked,
    )

    private fun render(state: ShareLinkDetailUiState) {
        compose.setThemedContent {
            ShareLinkDetailScreen(
                state = state,
                justCopied = false,
                onCopy = { events += "copy" },
                onRevoke = { events += "revoke" },
                onRetry = { events += "retry" },
                onBack = { events += "back" },
            )
        }
    }

    @Test
    fun `the rendered URL keeps its fragment`() {
        render(state())
        compose.onNodeWithTag(SHARE_DETAIL_URL_TAG).assertTextContains("#" + secret, substring = true)
    }

    @Test
    fun `the warning about the fragment is on screen beside the copy action`() {
        render(state())
        compose.onNodeWithText(string(R.string.android_sharing_copy_warning)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(SHARE_DETAIL_COPY_TAG).performScrollTo().performClick()
        assertEquals(listOf("copy"), events)
    }

    @Test
    fun `revoking asks first and only then reports`() {
        render(state())
        compose.onNodeWithTag(SHARE_DETAIL_REVOKE_TAG).performScrollTo().performClick()
        compose.onNodeWithText(string(R.string.android_sharing_revoke_confirm_title)).assertIsDisplayed()
        assertEquals(emptyList<String>(), events)
        compose.onNodeWithText(string(R.string.android_sharing_revoke_confirm_action)).performClick()
        assertEquals(listOf("revoke"), events)
    }

    @Test
    fun `a link with no secret on this device explains itself instead of offering a copy`() {
        render(state(url = null))
        compose.onNodeWithText(string(R.string.android_sharing_secret_unavailable_title)).assertIsDisplayed()
        assertEquals(0, compose.onAllNodesWithTag(SHARE_DETAIL_COPY_TAG).fetchSemanticsNodes().size)
    }

    @Test
    fun `a revoked link says revoked`() {
        render(state(url = null, isRevoked = true))
        compose.onNodeWithText(string(R.string.android_sharing_revoked_title)).assertIsDisplayed()
    }

    @Test
    fun `a link that vanished is explained as no longer live`() {
        compose.setThemedContent {
            ShareLinkDetailScreen(
                state = ShareLinkDetailUiState(isLoading = false),
                justCopied = false,
                onCopy = {},
                onRevoke = {},
                onRetry = {},
                onBack = { events += "back" },
            )
        }
        compose.onNodeWithText(string(R.string.android_sharing_detail_missing_title)).assertIsDisplayed()
    }

    @Test
    fun `a revocation in flight disables the button that started it`() {
        render(state().copy(isRevoking = true))
        compose.onNodeWithTag(SHARE_DETAIL_REVOKE_TAG).performScrollTo().assertIsNotEnabled()
    }
}
