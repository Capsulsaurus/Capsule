package com.justin13888.capsule.feature.recovery

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
import com.justin13888.capsule.core.data.EscrowError
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the local check draws.
 *
 * The two states that must never be confused are asserted separately: a miss, which is a
 * warning that says nothing was sent and nothing is locked, and an unavailable check, which is
 * an error about the escrow rather than about the passphrase. Telling someone the first when
 * the second happened is how a correct phrase gets thrown away.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class RecoveryVerifyScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun the_form_says_the_check_is_local_before_anything_is_typed() {
        render(RecoveryVerifyUiState())

        compose.onNodeWithText(BODY).assertIsDisplayed()
        compose.onNodeWithText(OFFLINE_NOTE).assertIsDisplayed()
    }

    @Test
    fun typing_the_passphrase_reports_what_was_typed() {
        val typed = mutableListOf<String>()
        render(RecoveryVerifyUiState(revealed = true), onSecretChange = typed::add)

        compose.onNodeWithTag(RECOVERY_VERIFY_FIELD_TAG).performTextReplacement(PHRASE)

        assertEquals(listOf(PHRASE), typed)
    }

    @Test
    fun an_empty_field_says_what_is_missing() {
        render(RecoveryVerifyUiState(secretMissing = true))

        compose.onNodeWithText(EMPTY).assertIsDisplayed()
    }

    @Test
    fun a_check_in_flight_shows_the_loading_state() {
        render(RecoveryVerifyUiState(phase = VerifyPhase.Checking))

        compose.onNodeWithText(CHECKING).assertIsDisplayed()
    }

    @Test
    fun a_miss_says_nothing_was_sent_and_nothing_is_locked() {
        render(RecoveryVerifyUiState(phase = VerifyPhase.NotVerified, consecutiveFailures = 1))

        compose.onNodeWithTag(RECOVERY_VERIFY_MISS_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(MISS_BODY).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(STALE_CACHE_NOTE).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_first_miss_does_not_offer_a_new_secret_yet() {
        render(RecoveryVerifyUiState(phase = VerifyPhase.NotVerified, consecutiveFailures = 1))

        compose.onNodeWithText(REWRAP_ACTION).assertDoesNotExist()
    }

    @Test
    fun enough_misses_offer_a_new_secret_beside_the_still_live_field() {
        var rewrapped = 0
        render(
            RecoveryVerifyUiState(phase = VerifyPhase.NotVerified, consecutiveFailures = 3),
            onRewrap = { rewrapped++ },
        )

        compose.onNodeWithTag(RECOVERY_VERIFY_FIELD_TAG).assertIsDisplayed()
        compose.onNodeWithText(REWRAP_ACTION).performScrollTo().performClick()

        assertEquals(1, rewrapped)
    }

    @Test
    fun a_verified_phrase_names_the_next_prompt() {
        render(
            RecoveryVerifyUiState(
                phase = VerifyPhase.Verified,
                nextPromptAt = RecoveryFixtures.NOW.plusSeconds(SECONDS_PER_DAY * 90),
            ),
        )

        compose.onNodeWithTag(RECOVERY_VERIFY_OK_TAG).assertIsDisplayed()
        compose.onNodeWithText(VERIFIED_TITLE).assertIsDisplayed()
    }

    @Test
    fun an_unavailable_check_is_an_error_about_the_escrow_and_not_about_the_phrase() {
        var reset = 0
        render(
            RecoveryVerifyUiState(phase = VerifyPhase.Failed, failure = RecoveryFailure(EscrowError.Malformed.code)),
            onReset = { reset++ },
        )

        compose.onNodeWithTag(RECOVERY_VERIFY_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(ESCROW_MALFORMED).assertIsDisplayed()
        compose.onNodeWithText(MISS_TITLE).assertDoesNotExist()
        compose.onNodeWithText(RETRY).performClick()

        assertEquals(1, reset)
    }

    @Test
    fun revealing_the_phrase_is_offered_and_reports_the_change() {
        val toggles = mutableListOf<Boolean>()
        render(RecoveryVerifyUiState(), onRevealChange = toggles::add)

        compose.onNodeWithText(REVEAL).performScrollTo().performClick()

        assertEquals(listOf(true), toggles)
    }

    @Test
    fun the_form_passes_the_accessibility_checks_the_design_system_promises() {
        render(RecoveryVerifyUiState(secret = PHRASE, revealed = true))

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun render(
        state: RecoveryVerifyUiState,
        onSecretChange: (String) -> Unit = {},
        onRevealChange: (Boolean) -> Unit = {},
        onReset: () -> Unit = {},
        onRewrap: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                RecoveryVerifyScreen(
                    state = state,
                    onSecretChange = onSecretChange,
                    onRevealChange = onRevealChange,
                    onVerify = {},
                    onReset = onReset,
                    onRewrap = onRewrap,
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

        const val SECONDS_PER_DAY = 86_400L
        const val PHRASE = "correct horse battery staple"
        const val BODY =
            "Type your recovery passphrase. The check runs on this device only — nothing is sent " +
                "anywhere, and getting it wrong locks nothing."
        const val OFFLINE_NOTE = "Works with no connection."
        const val EMPTY = "Type your passphrase first."
        const val CHECKING = "Checking on this device…"
        const val MISS_TITLE = "That didn't match"
        const val MISS_BODY =
            "Attempt 1 of 3. Nothing was sent and nothing is locked — check for a typo, or set a " +
                "secret you do have."
        const val STALE_CACHE_NOTE =
            "Your recovery secret may have been replaced from another device. Re-reading it and " +
                "trying once more before calling this a miss."
        const val REWRAP_ACTION = "Set a new recovery secret"
        const val VERIFIED_TITLE = "That's the one"
        const val ESCROW_MALFORMED = "The recovery backup could not be saved."
        const val RETRY = "Try again"
        const val REVEAL = "Show the passphrase"
    }
}
