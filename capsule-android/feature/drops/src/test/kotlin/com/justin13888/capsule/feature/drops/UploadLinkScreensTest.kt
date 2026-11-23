package com.justin13888.capsule.feature.drops

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextReplacement
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.LinkCaps
import com.justin13888.capsule.feature.drops.DropsScreenHarness.setThemedContent
import com.justin13888.capsule.feature.drops.DropsScreenHarness.string
import com.justin13888.capsule.feature.drops.ui.UPLOAD_CREATE_COUNT_TAG
import com.justin13888.capsule.feature.drops.ui.UPLOAD_CREATE_SINGLE_USE_TAG
import com.justin13888.capsule.feature.drops.ui.UPLOAD_CREATE_SUBMIT_TAG
import com.justin13888.capsule.feature.drops.ui.UPLOAD_DETAIL_BYTES_TAG
import com.justin13888.capsule.feature.drops.ui.UPLOAD_DETAIL_FILES_TAG
import com.justin13888.capsule.feature.drops.ui.UPLOAD_DETAIL_REVOKE_TAG
import com.justin13888.capsule.feature.drops.ui.UPLOAD_LINK_ROW_TAG
import com.justin13888.capsule.feature.drops.ui.UploadLinkCreateActions
import com.justin13888.capsule.feature.drops.ui.UploadLinkCreateScreen
import com.justin13888.capsule.feature.drops.ui.UploadLinkDetailScreen
import com.justin13888.capsule.feature.drops.ui.UploadLinksScreen
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The three upload-link screens.
 *
 * The caps are what these tests are really about. On the form they must be enterable and,
 * when nonsense is typed, refused rather than rounded; on the detail they must be readable
 * against what has already been spent — that pair is the whole of what bounds a leaked link.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class UploadLinkScreensTest {
    @get:Rule
    val compose = createComposeRule()

    private val events = mutableListOf<String>()

    private val createActions =
        UploadLinkCreateActions(
            selectExpiry = { events += "expiry:" + it.name },
            setCap = { kind, raw -> events += "cap:" + kind.name + ":" + raw },
            setSingleUse = { events += "single:" + it },
            setPassphrase = { events += "passphrase:" + it },
            submit = { events += "submit" },
            back = { events += "back" },
        )

    // ---- list ---------------------------------------------------------------------------

    @Test
    fun `the list loading state names what it is waiting for`() {
        compose.setThemedContent {
            UploadLinksScreen(UploadLinksUiState(), {}, {}, {}, {})
        }
        compose.onNodeWithText(string(R.string.android_drops_links_loading)).assertIsDisplayed()
    }

    @Test
    fun `an empty list says an upload link grants no view access`() {
        compose.setThemedContent {
            UploadLinksScreen(UploadLinksUiState(isLoading = false), {}, {}, {}, {})
        }
        compose.onNodeWithText(string(R.string.android_drops_links_empty_body)).assertIsDisplayed()
    }

    @Test
    fun `a row leads with what has arrived and opens that link`() {
        val row = UploadLinkRow(DropsScreenHarness.uploadLink(), acceptsDeposits = true)
        compose.setThemedContent {
            UploadLinksScreen(
                state = UploadLinksUiState(isLoading = false, links = listOf(row)),
                onOpenLink = { events += "open:" + it.value },
                onCreate = {},
                onRetry = {},
                onBack = {},
            )
        }
        compose.onNodeWithText(string(R.string.android_drops_link_open)).assertIsDisplayed()
        compose.onNodeWithTag(UPLOAD_LINK_ROW_TAG + row.link.opaqueId).performClick()
        assertEquals(listOf("open:upload-link-under-test"), events)
    }

    @Test
    fun `a closed link is marked as no longer accepting uploads`() {
        val row = UploadLinkRow(DropsScreenHarness.uploadLink(), acceptsDeposits = false)
        compose.setThemedContent {
            UploadLinksScreen(UploadLinksUiState(isLoading = false, links = listOf(row)), {}, {}, {}, {})
        }
        compose.onNodeWithText(string(R.string.android_drops_link_closed)).assertIsDisplayed()
    }

    // ---- create -------------------------------------------------------------------------

    @Test
    fun `the form explains that the caps bound what a leaked link can cost`() {
        compose.setThemedContent {
            UploadLinkCreateScreen(state = UploadLinkCreateUiState(), actions = createActions)
        }
        compose.onNodeWithText(string(R.string.android_drops_create_caps_body)).assertIsDisplayed()
    }

    @Test
    fun `typing a cap reports which cap it was`() {
        compose.setThemedContent {
            UploadLinkCreateScreen(state = UploadLinkCreateUiState(), actions = createActions)
        }
        compose.onNodeWithTag(UPLOAD_CREATE_COUNT_TAG).performScrollTo().performTextReplacement("20")
        assertEquals(listOf("cap:FileCount:20"), events)
    }

    @Test
    fun `an empty cap is presented as no limit rather than as a blank to fill`() {
        compose.setThemedContent {
            UploadLinkCreateScreen(state = UploadLinkCreateUiState(), actions = createActions)
        }
        // One under every numeric cap, because every one of them means the same thing empty.
        val hints = compose.onAllNodesWithText(string(R.string.android_drops_create_unbounded_hint))
        assertEquals(3, hints.fetchSemanticsNodes().size)
        hints[0].performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `a cap that does not parse is explained and blocks submission`() {
        compose.setThemedContent {
            UploadLinkCreateScreen(
                state = UploadLinkCreateUiState(maxFiles = CapEntry("twenty")),
                actions = createActions,
            )
        }
        compose
            .onNodeWithText(string(R.string.android_drops_create_invalid_number))
            .performScrollTo()
            .assertIsDisplayed()
        compose.onNodeWithTag(UPLOAD_CREATE_SUBMIT_TAG).performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun `single use is off until it is chosen`() {
        compose.setThemedContent {
            UploadLinkCreateScreen(state = UploadLinkCreateUiState(), actions = createActions)
        }
        compose.onNodeWithTag(UPLOAD_CREATE_SINGLE_USE_TAG).performScrollTo().assertIsOff()
        compose.onNodeWithTag(UPLOAD_CREATE_SINGLE_USE_TAG).performClick()
        assertEquals(listOf("single:true"), events)
    }

    @Test
    fun `the passphrase is described as an abuse gate rather than as privacy`() {
        compose.setThemedContent {
            UploadLinkCreateScreen(state = UploadLinkCreateUiState(), actions = createActions)
        }
        compose
            .onNodeWithText(string(R.string.android_drops_create_passphrase_help))
            .performScrollTo()
            .assertIsDisplayed()
    }

    // ---- detail -------------------------------------------------------------------------

    @Test
    fun `the detail draws a bar for a cap that has a ceiling`() {
        compose.setThemedContent {
            UploadLinkDetailScreen(
                state =
                    UploadLinkDetailUiState(
                        isLoading = false,
                        link = DropsScreenHarness.uploadLink(),
                        acceptsDeposits = true,
                    ),
                onRevoke = {},
                onRetry = {},
                onBack = {},
            )
        }
        compose.onNodeWithTag(UPLOAD_DETAIL_BYTES_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(UPLOAD_DETAIL_FILES_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `an unbounded cap is stated as having no limit instead of drawn as empty`() {
        compose.setThemedContent {
            UploadLinkDetailScreen(
                state =
                    UploadLinkDetailUiState(
                        isLoading = false,
                        link = DropsScreenHarness.uploadLink(caps = LinkCaps()),
                        acceptsDeposits = true,
                    ),
                onRevoke = {},
                onRetry = {},
                onBack = {},
            )
        }
        // Both the byte and the file cap read this way when neither ceiling was set.
        val hint = string(R.string.android_drops_usage_unbounded).substringAfter("{used}")
        val summaries = compose.onAllNodesWithText(hint, substring = true)
        assertEquals(2, summaries.fetchSemanticsNodes().size)
        summaries[0].performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(string(R.string.android_drops_cap_file_size_unbounded)).assertIsDisplayed()
    }

    @Test
    fun `an expired link says why it stopped accepting uploads`() {
        compose.setThemedContent {
            UploadLinkDetailScreen(
                state =
                    UploadLinkDetailUiState(
                        isLoading = false,
                        link = DropsScreenHarness.uploadLink(),
                        acceptsDeposits = false,
                        hasExpired = true,
                    ),
                onRevoke = {},
                onRetry = {},
                onBack = {},
            )
        }
        compose.onNodeWithText(string(R.string.android_drops_closed_expired)).assertIsDisplayed()
    }

    @Test
    fun `revoking asks first, and says the inbox is unaffected`() {
        compose.setThemedContent {
            UploadLinkDetailScreen(
                state =
                    UploadLinkDetailUiState(
                        isLoading = false,
                        link = DropsScreenHarness.uploadLink(),
                        acceptsDeposits = true,
                    ),
                onRevoke = { events += "revoke" },
                onRetry = {},
                onBack = {},
            )
        }
        compose.onNodeWithTag(UPLOAD_DETAIL_REVOKE_TAG).performScrollTo().performClick()
        compose.onNodeWithText(string(R.string.android_drops_revoke_confirm_body)).assertIsDisplayed()
        assertEquals(emptyList<String>(), events)
        compose.onNodeWithText(string(R.string.android_drops_revoke_confirm_action)).performClick()
        assertEquals(listOf("revoke"), events)
    }

    @Test
    fun `a link that vanished is explained rather than shown blank`() {
        compose.setThemedContent {
            UploadLinkDetailScreen(
                state = UploadLinkDetailUiState(isLoading = false),
                onRevoke = {},
                onRetry = {},
                onBack = {},
            )
        }
        compose.onNodeWithText(string(R.string.android_drops_detail_missing_title)).assertIsDisplayed()
    }
}
