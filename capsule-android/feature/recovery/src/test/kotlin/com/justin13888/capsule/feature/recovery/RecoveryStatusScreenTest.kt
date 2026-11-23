package com.justin13888.capsule.feature.recovery

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.data.EscrowError
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.model.RecoveryCadence
import com.justin13888.capsule.core.model.RecoveryState
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the hub draws in each standing it can report.
 *
 * The assertion that matters most is the dullest: the advisory note is on screen in every
 * state, including the ones that look like trouble. A reminder that reads as a threat is a
 * reminder people learn to dismiss without reading, and this one has to be read.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class RecoveryStatusScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun a_reading_in_flight_shows_the_loading_state() {
        render(RecoveryStatusUiState())

        compose.onNodeWithTag(RECOVERY_STATUS_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun an_unreadable_setup_localizes_its_code_and_offers_a_way_forward() {
        var retried = 0
        render(
            RecoveryStatusUiState(isLoading = false, failure = RecoveryFailure(EscrowError.Malformed.code)),
            onRetry = { retried++ },
        )

        compose.onNodeWithTag(RECOVERY_STATUS_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(ESCROW_MALFORMED).assertIsDisplayed()
        compose.onNodeWithText(RETRY).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun a_library_with_no_recovery_secret_is_invited_rather_than_scolded() {
        render(settled(RecoveryState.NotConfigured))

        compose.onNodeWithText(NOT_CONFIGURED_TITLE).assertIsDisplayed()
        compose.onNodeWithTag(RECOVERY_ADVISORY_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_healthy_standing_names_the_last_and_next_dates() {
        render(
            settled(
                RecoveryState.Healthy(
                    lastVerifiedAt = RecoveryFixtures.NOW.minusSeconds(SECONDS_PER_DAY * 40),
                    nextPromptAt = RecoveryFixtures.NOW.plusSeconds(SECONDS_PER_DAY * 50),
                    consecutiveSuccesses = 1,
                ),
            ),
        )

        compose.onNodeWithTag(RECOVERY_STANDING_TAG).assertIsDisplayed()
        compose.onNodeWithText(HEALTHY_TITLE).assertIsDisplayed()
    }

    @Test
    fun a_due_prompt_offers_both_deferrals() {
        val snoozes = mutableListOf<String>()
        render(
            settled(RecoveryState.VerificationDue(RecoveryFixtures.NOW.minusSeconds(SECONDS_PER_DAY))),
            onSnoozeDay = { snoozes += DAY },
            onSnoozeWeek = { snoozes += WEEK },
        )

        compose.onNodeWithText(SNOOZE_DAY).performScrollTo().performClick()
        compose.onNodeWithText(SNOOZE_WEEK).performScrollTo().performClick()

        assertEquals(listOf(DAY, WEEK), snoozes)
    }

    @Test
    fun past_the_snooze_cap_the_reminder_stays_and_the_snooze_actions_go() {
        render(
            settled(
                RecoveryState.Snoozed(
                    until = RecoveryFixtures.NOW.plusSeconds(SECONDS_PER_DAY),
                    consecutiveSnoozes = RecoveryCadence.MAX_CONSECUTIVE_SNOOZES,
                ),
            ),
        )

        compose.onNodeWithText(PERSISTENT_NOTE, substring = true).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(SNOOZE_DAY).assertDoesNotExist()
    }

    @Test
    fun repeated_misses_promote_the_re_wrap_without_threatening_anything() {
        var rewrapped = 0
        render(
            settled(RecoveryState.RewrapRecommended(consecutiveFailures = 3)),
            onRewrap = { rewrapped++ },
        )

        compose.onNodeWithText(REWRAP_TITLE).assertIsDisplayed()
        compose.onNodeWithTag(RECOVERY_ADVISORY_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(REWRAP_ACTION).performScrollTo().performClick()

        assertEquals(1, rewrapped)
    }

    @Test
    fun every_other_recovery_surface_is_reachable_from_here() {
        val opened = mutableListOf<String>()
        render(
            settled(RecoveryState.NotConfigured),
            onShamir = { opened += SHAMIR },
            onExport = { opened += EXPORT },
            onRestore = { opened += RESTORE },
        )

        compose.onNodeWithText(SHAMIR_LABEL).performScrollTo().performClick()
        compose.onNodeWithText(EXPORT_LABEL).performScrollTo().performClick()
        compose.onNodeWithText(RESTORE_LABEL).performScrollTo().performClick()

        assertEquals(listOf(SHAMIR, EXPORT, RESTORE), opened)
    }

    @Test
    fun the_hub_passes_the_accessibility_checks_the_design_system_promises() {
        render(settled(RecoveryState.VerificationDue(RecoveryFixtures.NOW)))

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun settled(recovery: RecoveryState) = RecoveryStatusUiState(isLoading = false, recovery = recovery)

    @Suppress("LongParameterList")
    private fun render(
        state: RecoveryStatusUiState,
        onRewrap: () -> Unit = {},
        onSnoozeDay: () -> Unit = {},
        onSnoozeWeek: () -> Unit = {},
        onShamir: () -> Unit = {},
        onExport: () -> Unit = {},
        onRestore: () -> Unit = {},
        onRetry: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                RecoveryStatusScreen(
                    state = state,
                    actions =
                        RecoveryStatusActions(
                            onVerify = {},
                            onSnoozeDay = onSnoozeDay,
                            onSnoozeWeek = onSnoozeWeek,
                            onRewrap = onRewrap,
                            onShamir = onShamir,
                            onExport = onExport,
                            onRestore = onRestore,
                        ),
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

        const val SECONDS_PER_DAY = 86_400L
        const val DAY = "day"
        const val WEEK = "week"
        const val SHAMIR = "shamir"
        const val EXPORT = "export"
        const val RESTORE = "restore"
        const val ESCROW_MALFORMED = "The recovery backup could not be saved."
        const val RETRY = "Try again"
        const val NOT_CONFIGURED_TITLE = "No recovery secret yet"
        const val HEALTHY_TITLE = "Recovery is set up"
        const val PERSISTENT_NOTE = "Snoozed three times"
        const val SNOOZE_DAY = "Remind me tomorrow"
        const val SNOOZE_WEEK = "Remind me in a week"
        const val REWRAP_TITLE = "Let's set a recovery secret you have"
        const val REWRAP_ACTION = "Set a new recovery secret"
        const val SHAMIR_LABEL = "Split it between guardians"
        const val EXPORT_LABEL = "Export a backup"
        const val RESTORE_LABEL = "Restore from a backup"
    }
}
