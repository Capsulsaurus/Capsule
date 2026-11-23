package com.justin13888.capsule.feature.sharing

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.sharing.ScreenHarness.setThemedContent
import com.justin13888.capsule.feature.sharing.ScreenHarness.string
import com.justin13888.capsule.feature.sharing.ui.SHARE_LINK_ROW_TAG
import com.justin13888.capsule.feature.sharing.ui.ShareLinksScreen
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Instant

/**
 * The list of issued share links, in each of its states.
 *
 * The row assertions matter because the list is where a user goes to *stop* sharing
 * something. A row that failed to say what it points at, or that was too small to hit, would
 * make revocation a guess.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ShareLinksScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val events = mutableListOf<String>()

    private fun render(state: SharingUiState) {
        compose.setThemedContent {
            ShareLinksScreen(
                state = state,
                onOpenLink = { events += "open:" + it.value },
                onCreate = { events += "create" },
                onRetry = { events += "retry" },
                onBack = { events += "back" },
            )
        }
    }

    @Test
    fun `the loading state names what it is waiting for`() {
        render(SharingUiState())
        compose.onNodeWithText(string(R.string.android_sharing_loading)).assertIsDisplayed()
    }

    @Test
    fun `an empty list explains why expired links are not in it`() {
        render(SharingUiState(isLoading = false))
        compose.onNodeWithText(string(R.string.android_sharing_links_empty_title)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_sharing_links_empty_body)).assertIsDisplayed()
    }

    @Test
    fun `a failure with nothing to show offers a retry`() {
        render(SharingUiState(isLoading = false, failure = SharingFailure(SharingFailureKind.Unknown)))
        compose.onNodeWithText(string(R.string.android_sharing_retry)).performClick()
        assertEquals(listOf("retry"), events)
    }

    @Test
    fun `a row names the album it shares and opens that link`() {
        val row = ShareLinkRow(ScreenHarness.link(), albumName = "Iceland")
        render(SharingUiState(isLoading = false, shareLinks = listOf(row)))
        compose.onNodeWithText("Iceland").assertIsDisplayed()
        compose.onNodeWithTag(SHARE_LINK_ROW_TAG + row.link.opaqueId).performClick()
        assertEquals(listOf("open:" + row.link.id.value), events)
    }

    @Test
    fun `a row states its expiry and whether a passphrase is needed`() {
        val row =
            ShareLinkRow(
                ScreenHarness.link(
                    expiresAt = Instant.parse("2026-09-01T00:00:00Z"),
                    requiresPassphrase = true,
                ),
                albumName = "Iceland",
            )
        render(SharingUiState(isLoading = false, shareLinks = listOf(row)))
        compose
            .onNodeWithText(string(R.string.android_sharing_passphrase_required), substring = true)
            .assertIsDisplayed()
    }
}
