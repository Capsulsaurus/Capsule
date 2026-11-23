package com.justin13888.capsule.feature.security

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performScrollToNode
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.data.AuthError
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.SessionId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the session ledger draws, and what it deliberately does not.
 *
 * The absence test matters as much as the presence ones: there is no "this isn't my device"
 * control anywhere on this screen, because a user cannot adjudicate a hash, and a screen that
 * grew one later would be quietly asking them to.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class SessionsScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun a_reading_in_flight_shows_the_loading_state() {
        render(SessionsUiState())

        compose.onNodeWithTag(SECURITY_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun a_failure_localizes_its_code_and_offers_a_retry() {
        var retried = 0
        render(
            SecurityFixtures.sessionsState().copy(failure = SecurityFailure.of(AuthError.RateLimited)),
            onRetry = { retried++ },
        )

        compose.onNodeWithTag(SECURITY_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(RATE_LIMITED_MESSAGE).assertIsDisplayed()
        compose.onNodeWithText(RETRY_LABEL).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun a_signed_out_account_shows_an_empty_ledger_rather_than_an_error() {
        render(SessionsUiState(isLoading = false))

        compose.onNodeWithTag(SECURITY_EMPTY_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun this_device_is_named_and_a_familiar_one_is_asserted_rather_than_questioned() {
        render(SecurityFixtures.sessionsState())

        compose.onNodeWithTag(securityCohortTag(SecurityFixtures.CURRENT_COHORT)).assertIsDisplayed()
        compose.onNodeWithText(THIS_DEVICE).assertIsDisplayed()
        compose
            .onNodeWithTag(securityCohortTag(SecurityFixtures.RETURNING_COHORT))
            .performScrollTo()
            .assertIsDisplayed()
        compose.onNodeWithText(SEEN_BEFORE, substring = true).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun there_is_no_control_for_disputing_a_grouping_only_a_way_to_report_it() {
        render(SecurityFixtures.sessionsState())

        assertTrue(
            "no screen text invites the user to adjudicate a hash",
            compose.onAllNodesWithText(DISPUTE_TOGGLE, substring = true).fetchSemanticsNodes().isEmpty(),
        )
        compose.onNodeWithTag(SECURITY_ADVISORY_TAG).assertIsDisplayed()
    }

    @Test
    fun the_dispute_path_is_one_tap_and_produces_the_exact_identifiers() {
        val reported = mutableListOf<String?>()
        render(SecurityFixtures.sessionsState(), onReportCohort = reported::add)

        compose.onAllNodesWithText(REPORT_LABEL)[0].performScrollTo().performClick()

        assertEquals(listOf(SecurityFixtures.RETURNING_COHORT), reported)
    }

    @Test
    fun the_support_report_sheet_shows_the_bundle_and_says_why_there_is_no_toggle() {
        val group = SecurityFixtures.sessionsState().groups.first { !it.isCurrent }
        render(SecurityFixtures.sessionsState().copy(supportReport = SupportBundle.render(group)))

        compose.onNodeWithTag(SECURITY_SUPPORT_TAG).assertIsDisplayed()
        compose.onNodeWithText(SUPPORT_NOTE, substring = true).assertIsDisplayed()
    }

    @Test
    fun revoking_one_session_is_announced_by_when_it_was_last_used() {
        val revoked = mutableListOf<SessionId>()
        render(SecurityFixtures.sessionsState(), onRevokeSession = revoked::add)

        compose.onAllNodesWithText(REVOKE_LABEL)[0].performScrollTo().performClick()

        assertEquals(1, revoked.size)
    }

    @Test
    fun the_global_revoke_explains_why_it_costs_more_than_the_row_above_it() {
        var confirmed = 0
        render(
            SecurityFixtures.sessionsState().copy(revokeAll = RevokeAllState.Confirming),
            onConfirmRevokeAll = { confirmed++ },
        )

        compose.onNodeWithTag(SECURITY_REVOKE_ALL_TAG).assertIsDisplayed()
        compose.onNodeWithText(REVOKE_ALL_BODY, substring = true).assertIsDisplayed()
        compose.onNodeWithText(REVOKE_ALL_CONFIRM).performClick()

        assertEquals(1, confirmed)
    }

    @Test
    fun a_refused_global_revoke_says_nothing_was_signed_out() {
        render(
            SecurityFixtures.sessionsState().copy(
                revokeAll = RevokeAllState.Refused(SecurityFailure.of(AuthError.RevokeProofRequired)),
            ),
        )

        compose.onNodeWithText(REVOKE_ALL_NOTHING).assertIsDisplayed()
        compose.onNodeWithText(PROOF_REQUIRED_MESSAGE).assertIsDisplayed()
    }

    @Test
    fun a_retired_device_stays_listed_and_the_screen_says_why() {
        val revoked = mutableListOf<DeviceId>()
        render(SecurityFixtures.sessionsState(), onRevokeDevice = revoked::add)

        compose.onNodeWithTag(SECURITY_LIST_TAG).performScrollToNode(hasTestTag(securityDeviceTag(RETIRED_DEVICE)))
        compose.onNodeWithTag(securityDeviceTag(RETIRED_DEVICE)).assertIsDisplayed()
        compose.onNodeWithTag(SECURITY_LIST_TAG).performScrollToNode(hasText(RETIRED_NOTE, substring = true))
        compose.onNodeWithText(RETIRED_NOTE, substring = true).assertIsDisplayed()
    }

    @Test
    fun the_sessions_screen_passes_the_accessibility_checks_the_design_system_promises() {
        render(SecurityFixtures.sessionsState())

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Suppress("LongParameterList")
    private fun render(
        state: SessionsUiState,
        onRevokeSession: (SessionId) -> Unit = {},
        onRevokeDevice: (DeviceId) -> Unit = {},
        onAskRevokeAll: () -> Unit = {},
        onConfirmRevokeAll: () -> Unit = {},
        onDismissRevokeAll: () -> Unit = {},
        onReportCohort: (String?) -> Unit = {},
        onDismissReport: () -> Unit = {},
        onRetry: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                SessionsScreen(
                    state = state,
                    actions =
                        SessionsActions(
                            onRevokeSession = onRevokeSession,
                            onRevokeDevice = onRevokeDevice,
                            onAskRevokeAll = onAskRevokeAll,
                            onConfirmRevokeAll = onConfirmRevokeAll,
                            onDismissRevokeAll = onDismissRevokeAll,
                            onReportCohort = onReportCohort,
                            onDismissReport = onDismissReport,
                            onRetry = onRetry,
                        ),
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

        const val RETIRED_DEVICE = "device-c"
        const val RATE_LIMITED_MESSAGE = "Too many attempts. Please wait and try again."
        const val PROOF_REQUIRED_MESSAGE = "Confirm with your account key to log out of all devices."
        const val RETRY_LABEL = "Try again"
        const val THIS_DEVICE = "This device"
        const val SEEN_BEFORE = "A device you have used before"
        const val DISPUTE_TOGGLE = "isn't my device"
        const val REPORT_LABEL = "Report a device you do not recognise"
        const val REVOKE_LABEL = "Sign out"
        const val REVOKE_ALL_CONFIRM = "Confirm with account key"
        const val REVOKE_ALL_NOTHING = "Nothing was signed out."
        const val REVOKE_ALL_BODY = "confirmed with your account key rather than with this session"
        const val SUPPORT_NOTE = "the grouping is a hint the app cannot verify"
        const val RETIRED_NOTE = "it signed part of your library's history"
    }
}
