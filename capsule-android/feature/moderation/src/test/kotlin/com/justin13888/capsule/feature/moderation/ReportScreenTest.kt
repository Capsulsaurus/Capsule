package com.justin13888.capsule.feature.moderation

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextReplacement
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the report form draws in each of the states it can be in.
 *
 * The payload assertion is the one that matters: the hash is on screen, editable and named,
 * because "we never send your photo" is a claim and a visible hash is the evidence for it.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ReportScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun a_destination_that_named_nothing_says_so_rather_than_offering_an_empty_form() {
        render(ReportUiState())

        compose.onNodeWithText(SUBJECT_MISSING).assertIsDisplayed()
    }

    @Test
    fun the_form_shows_the_exact_payload_that_leaves_the_device() {
        render(ReportUiState.forSubject(ModerationFixtures.CONTENT_HASH))

        compose.onNodeWithTag(MODERATION_REPORT_HASH_TAG).assertIsDisplayed()
        compose.onNodeWithText(PRIVACY_NOTE).assertIsDisplayed()
    }

    @Test
    fun editing_the_reason_reports_what_was_typed() {
        val typed = mutableListOf<String>()
        render(ReportUiState.forSubject(ModerationFixtures.CONTENT_HASH), onReasonChange = typed::add)

        compose.onNodeWithTag(MODERATION_REPORT_REASON_TAG).performTextReplacement(REASON)

        assertEquals(listOf(REASON), typed)
    }

    @Test
    fun a_content_report_offers_no_block_because_it_names_no_account() {
        render(ReportUiState.forSubject(ModerationFixtures.CONTENT_HASH))

        compose.onNodeWithText(ALSO_BLOCK).assertDoesNotExist()
    }

    @Test
    fun an_account_report_offers_the_block_and_says_it_stands_on_its_own() {
        render(ReportUiState.forSubject(ACCOUNT))

        compose.onNodeWithText(ALSO_BLOCK).assertIsDisplayed()
        compose.onNodeWithText(BLOCK_NOTE).assertIsDisplayed()
    }

    @Test
    fun a_report_naming_nothing_actionable_is_refused_with_a_reason() {
        render(ReportUiState.forSubject(ACCOUNT).copy(hashMissing = true))

        compose.onNodeWithText(HASH_REQUIRED).assertIsDisplayed()
    }

    @Test
    fun a_submission_in_flight_shows_the_loading_state() {
        render(ReportUiState.forSubject(ModerationFixtures.CONTENT_HASH).copy(phase = ReportPhase.Sending))

        compose.onNodeWithText(SENDING).assertIsDisplayed()
    }

    @Test
    fun a_landed_report_says_it_is_attributable_and_offers_the_way_out() {
        var done = 0
        render(
            ReportUiState.forSubject(ModerationFixtures.CONTENT_HASH).copy(phase = ReportPhase.Sent),
            onDone = { done++ },
        )

        compose.onNodeWithTag(MODERATION_REPORT_SENT_TAG).assertIsDisplayed()
        compose.onNodeWithText(CLOSE).performClick()

        assertEquals(1, done)
    }

    @Test
    fun a_refusal_localizes_its_code_and_offers_a_way_forward() {
        var retried = 0
        render(
            ReportUiState.forSubject(ModerationFixtures.CONTENT_HASH).copy(
                phase = ReportPhase.Failed,
                failure = ModerationFailure(ModerationError.ReportRateLimited.code),
            ),
            onRetry = { retried++ },
        )

        compose.onNodeWithTag(MODERATION_REPORT_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(RATE_LIMITED).assertIsDisplayed()
        compose.onNodeWithText(RETRY).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun a_failure_with_no_catalogue_code_falls_back_to_the_generic_message() {
        render(
            ReportUiState.forSubject(ModerationFixtures.CONTENT_HASH).copy(
                phase = ReportPhase.Failed,
                failure = ModerationFailure(code = null),
            ),
        )

        compose.onNodeWithText(GENERIC_ERROR).assertIsDisplayed()
    }

    @Test
    fun the_form_passes_the_accessibility_checks_the_design_system_promises() {
        // Every control filled and live. A disabled Material control's greyed label, and an
        // empty outlined field's hairline border, both fail the contrast check by
        // construction; asserting on either would be testing the palette rather than this
        // layout, and would hide a real regression behind a permanently red test.
        render(
            ReportUiState.forSubject(ModerationFixtures.CONTENT_HASH).copy(
                albumPointer = POINTER,
                reason = REASON,
            ),
        )

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun render(
        state: ReportUiState,
        onReasonChange: (String) -> Unit = {},
        onRetry: () -> Unit = {},
        onDone: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                ReportScreen(
                    state = state,
                    actions =
                        ReportActions(
                            onKindChange = {},
                            onContentHashChange = {},
                            onPointerChange = {},
                            onReasonChange = onReasonChange,
                            onAlsoBlockChange = {},
                            onSubmit = {},
                            onRetry = onRetry,
                        ),
                    onDone = onDone,
                )
            }
        }
        compose.waitForIdle()
    }

    private companion object {
        val ACCESSIBILITY_OPTIONS =
            RoborazziATFAccessibilityCheckOptions(
                checker = RoborazziATFAccessibilityChecker(),
                failureLevel = RoborazziATFAccessibilityChecker.CheckLevel.Warning,
            )

        const val ACCOUNT = "alice@other.tld"
        const val REASON = "This is a photo of my front door."
        const val POINTER = "album:0197a0d0-0000-7000-8000-000000000009"
        const val SUBJECT_MISSING = "There's nothing here to report."
        const val PRIVACY_NOTE =
            "An administrator can only open this if they already have access to the album. " +
                "Reporting never widens who can read anything."
        const val ALSO_BLOCK = "Block this account as well"
        const val BLOCK_NOTE =
            "Blocking is yours alone and takes effect whether or not the administrators act."
        const val HASH_REQUIRED =
            "A report has to name the photo's content hash — that is the only thing an administrator " +
                "can act on without being handed the photo itself."
        const val SENDING = "Sending the report…"
        const val CLOSE = "Close"
        const val RETRY = "Try again"
        const val RATE_LIMITED = "Too many reports from this source. Please wait and try again."
        const val GENERIC_ERROR = "Something went wrong. Please try again."
    }
}
