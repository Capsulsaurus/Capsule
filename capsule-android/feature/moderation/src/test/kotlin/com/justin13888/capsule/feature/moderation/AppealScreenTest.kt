package com.justin13888.capsule.feature.moderation

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextReplacement
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the moderation record and the appeal beneath it draw.
 *
 * Two assertions here are about honesty rather than about rendering: a takedown says the
 * user's bytes survive it, and a suspension says that signing every session out is still
 * available. Both are wordings the design fixes, and both are the kind of sentence that gets
 * quietly softened in an edit unless something fails when it does.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class AppealScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun a_reading_in_flight_shows_the_loading_state() {
        render(AppealUiState())

        compose.onNodeWithTag(MODERATION_APPEAL_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun a_clean_record_says_so_rather_than_showing_an_empty_column() {
        render(AppealUiState(isLoading = false))

        compose.onNodeWithTag(MODERATION_APPEAL_EMPTY_TAG).assertIsDisplayed()
    }

    @Test
    fun a_takedown_says_plainly_that_nothing_of_the_users_was_destroyed() {
        render(ModerationFixtures.appealState())

        compose.onNodeWithTag(moderationNoticeTag(ModerationFixtures.TAKEDOWN.id.value)).assertIsDisplayed()
        compose.onNodeWithText(TAKEDOWN_BODY).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(TAKEDOWN_REVERSIBLE).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_legal_hold_is_distinguished_from_an_ordinary_takedown() {
        render(ModerationFixtures.appealState(caseId = ModerationFixtures.LEGAL_HOLD.id.value))

        compose.onNodeWithText(LEGAL_HOLD_BODY).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_suspension_names_the_capability_a_compromised_account_needs_most() {
        render(ModerationFixtures.appealState())

        compose.onNodeWithTag(MODERATION_SUSPENSION_TAG).assertIsDisplayed()
        compose.onNodeWithText(CAN_REVOKE).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(CAN_READ).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun an_account_in_good_standing_gets_no_banner() {
        render(ModerationFixtures.appealState(effects = null))

        compose.onNodeWithTag(MODERATION_SUSPENSION_TAG).assertDoesNotExist()
    }

    @Test
    fun a_case_that_is_not_on_the_record_offers_no_form() {
        render(ModerationFixtures.appealState(caseId = "not-on-this-record"))

        compose.onNodeWithText(CASE_MISSING).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(MODERATION_APPEAL_STATEMENT_TAG).assertDoesNotExist()
    }

    @Test
    fun writing_a_statement_reports_what_was_typed() {
        val typed = mutableListOf<String>()
        render(ModerationFixtures.appealState(), onStatementChange = typed::add)

        compose.onNodeWithTag(MODERATION_APPEAL_STATEMENT_TAG).performScrollTo().performTextReplacement(STATEMENT)

        assertEquals(listOf(STATEMENT), typed)
    }

    @Test
    fun an_empty_statement_says_what_is_missing() {
        render(ModerationFixtures.appealState().copy(statementMissing = true))

        compose.onNodeWithText(STATEMENT_REQUIRED).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun gathering_the_proof_says_which_key_is_signing() {
        render(ModerationFixtures.appealState(phase = AppealPhase.Proving))

        compose.onNodeWithText(PROVING).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_rejected_proof_says_nothing_was_sent_rather_than_offering_a_blind_retry() {
        render(ModerationFixtures.appealState(phase = AppealPhase.Failed).copy(proofRejected = true))

        compose.onNodeWithTag(MODERATION_APPEAL_ERROR_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(PROOF_FAILED).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_filed_appeal_hands_back_a_reference_to_quote() {
        render(ModerationFixtures.appealState(phase = AppealPhase.Sent).copy(reference = REFERENCE))

        compose.onNodeWithTag(MODERATION_APPEAL_SENT_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(SENT_BODY).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_record_passes_the_accessibility_checks_the_design_system_promises() {
        render(ModerationFixtures.appealState())

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Test
    fun retrying_after_a_refusal_is_offered() {
        var retried = 0
        render(
            ModerationFixtures.appealState(phase = AppealPhase.Failed).copy(proofRejected = true),
            onRetry = { retried++ },
        )

        compose.onNodeWithText(RETRY).performScrollTo().performClick()

        assertEquals(1, retried)
    }

    private fun render(
        state: AppealUiState,
        onStatementChange: (String) -> Unit = {},
        onRetry: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                AppealScreen(
                    state = state,
                    onStatementChange = onStatementChange,
                    onSubmit = {},
                    onRetry = onRetry,
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

        const val STATEMENT = "Please look at this again."
        const val REFERENCE = "0000F1AB"
        const val TAKEDOWN_BODY =
            "This server stopped handing this photo to anyone who asks for it. Your copy is untouched — " +
                "the bytes are still yours, and your own backup still restores it."
        const val TAKEDOWN_REVERSIBLE =
            "A takedown can be lifted, and lifting it is its own entry on this record."
        const val LEGAL_HOLD_BODY =
            "Held for a legal obligation. It lifts when the obligation ends, not at an administrator's " +
                "discretion — and even then, none of your bytes are destroyed."
        const val CAN_REVOKE =
            "You can still sign every session out — that is gated by your master key, not by your " +
                "standing, so a suspension can never leave an intruder signed in."
        const val CAN_READ = "Everything already in your library still opens."
        const val CASE_MISSING = "That case isn't on your record."
        const val STATEMENT_REQUIRED = "Say something before you send it."
        const val PROVING = "Signing the challenge with your master key…"
        const val PROOF_FAILED = "Your master key didn't sign the challenge, so nothing was sent."
        const val SENT_BODY =
            "Reference 0000F1AB. The decision lands on your moderation record either way."
        const val RETRY = "Try again"
    }
}
