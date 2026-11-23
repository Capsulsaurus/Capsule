package com.justin13888.capsule.feature.security

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
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
 * The hub and the passkey list.
 *
 * The authenticator section is asserted in each of its three states, because they are three
 * different sentences: not set up, a secret handed over and unproved, and a live factor that
 * costs a code to turn off.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class SecurityScreensTest {
    @get:Rule
    val compose = createComposeRule()

    private val now = SecurityFixtures.NOW

    @Test
    fun the_hub_summarises_both_authorities_and_opens_each() {
        var sessions = 0
        var passkeys = 0
        renderHub(onOpenSessions = { sessions++ }, onOpenPasskeys = { passkeys++ })

        compose.onNodeWithTag(SECURITY_HUB_TAG).assertIsDisplayed()
        compose.onNodeWithText(SESSIONS_ROW).performClick()
        compose.onNodeWithText(PASSKEYS_ROW).performScrollTo().performClick()

        assertEquals(1, sessions)
        assertEquals(1, passkeys)
    }

    @Test
    fun an_account_with_no_authenticator_says_so_and_offers_to_set_one_up() {
        var begun = 0
        renderHub(totp = TotpActions({ begun++ }, {}, {}, {}, {}))

        compose.onNodeWithText(TOTP_NOT_SET_UP).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(TOTP_SETUP).performScrollTo().performClick()

        assertEquals(1, begun)
    }

    @Test
    fun an_enrollment_shows_the_secret_and_asks_for_a_code_before_it_is_live() {
        var confirmed = 0
        val typed = mutableListOf<String>()
        renderHub(
            credentials = SecurityFixtures.credentialsState(totp = enrollment(live = false)),
            totp = TotpActions({}, typed::add, { confirmed++ }, {}, {}),
        )

        compose.onNodeWithText(SECRET).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(SECURITY_CODE_FIELD_TAG).performScrollTo().performTextInput(CODE)
        compose.onNodeWithText(TOTP_CONFIRM).performScrollTo().performClick()

        assertEquals(listOf(CODE), typed)
        assertEquals(1, confirmed)
    }

    @Test
    fun a_live_authenticator_costs_a_code_to_turn_off() {
        var disabled = 0
        renderHub(
            credentials = SecurityFixtures.credentialsState(totp = enrollment(live = true)),
            totp = TotpActions({}, {}, {}, {}, { disabled++ }),
        )

        compose.onNodeWithText(TOTP_ENABLED).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(SECURITY_CODE_FIELD_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(TOTP_DISABLE).performScrollTo().performClick()

        assertEquals(1, disabled)
    }

    @Test
    fun a_rejected_code_is_explained_in_terms_of_the_code_rather_than_the_password() {
        renderHub(
            credentials =
                SecurityFixtures
                    .credentialsState(totp = enrollment(live = false))
                    .copy(failure = SecurityFailure.TotpCode),
        )

        compose.onNodeWithText(CODE_REJECTED).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_hub_passes_the_accessibility_checks_the_design_system_promises() {
        renderHub()

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Test
    fun a_reading_in_flight_shows_the_loading_state() {
        renderPasskeys(CredentialsUiState())

        compose.onNodeWithTag(SECURITY_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun an_account_with_no_passkeys_says_so_rather_than_showing_a_blank_list() {
        renderPasskeys(CredentialsUiState(isLoading = false))

        compose.onNodeWithTag(SECURITY_EMPTY_TAG).assertIsDisplayed()
    }

    @Test
    fun each_passkey_is_listed_with_when_it_was_added_and_whether_it_has_been_used() {
        renderPasskeys(SecurityFixtures.credentialsState())

        compose.onNodeWithTag(securityPasskeyTag("passkey-a")).assertIsDisplayed()
        compose.onNodeWithText(NEVER_USED, substring = true).assertIsDisplayed()
    }

    @Test
    fun deleting_a_passkey_reports_which_one() {
        val deleted = mutableListOf<PasskeyId>()
        renderPasskeys(SecurityFixtures.credentialsState(), onDeletePasskey = deleted::add)

        compose.onNodeWithText(DELETE_LABEL).performScrollTo().performClick()

        assertEquals(listOf(PasskeyId("passkey-a")), deleted)
    }

    @Test
    fun the_last_passkey_with_no_authenticator_is_warned_about() {
        renderPasskeys(SecurityFixtures.credentialsState())

        compose.onNodeWithText(LAST_FACTOR_WARNING, substring = true).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun adding_a_passkey_reports_the_request() {
        var added = 0
        renderPasskeys(SecurityFixtures.credentialsState(), onAddPasskey = { added++ })

        compose.onNodeWithText(ADD_LABEL).performScrollTo().performClick()

        assertEquals(1, added)
    }

    @Test
    fun the_passkeys_screen_passes_the_accessibility_checks_the_design_system_promises() {
        renderPasskeys(SecurityFixtures.credentialsState())

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun enrollment(live: Boolean) = TotpEnrollment(secret = SECRET, startedAt = now, isConfirmed = live)

    private fun renderHub(
        sessions: SessionsUiState = SecurityFixtures.sessionsState(),
        credentials: CredentialsUiState = SecurityFixtures.credentialsState(),
        totp: TotpActions = TotpActions({}, {}, {}, {}, {}),
        onOpenSessions: () -> Unit = {},
        onOpenPasskeys: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                SecurityHubScreen(
                    sessions = sessions,
                    credentials = credentials,
                    totp = totp,
                    onOpenSessions = onOpenSessions,
                    onOpenPasskeys = onOpenPasskeys,
                )
            }
        }
        compose.waitForIdle()
    }

    private fun renderPasskeys(
        state: CredentialsUiState,
        onAddPasskey: () -> Unit = {},
        onDeletePasskey: (PasskeyId) -> Unit = {},
        onRetry: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                PasskeysScreen(
                    state = state,
                    onAddPasskey = onAddPasskey,
                    onDeletePasskey = onDeletePasskey,
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

        const val SECRET = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP"
        const val CODE = "123456"
        const val SESSIONS_ROW = "Sessions and devices"
        const val PASSKEYS_ROW = "Passkeys"
        const val TOTP_NOT_SET_UP = "Not set up"
        const val TOTP_SETUP = "Set up authenticator app"
        const val TOTP_CONFIRM = "Confirm code"
        const val TOTP_ENABLED = "Authenticator app enabled."
        const val TOTP_DISABLE = "Disable TOTP"
        const val CODE_REJECTED = "That code did not match. Codes are six digits and change every 30 seconds."
        const val NEVER_USED = "Never used to sign in"
        const val DELETE_LABEL = "Delete"
        const val ADD_LABEL = "Add passkey"
        const val LAST_FACTOR_WARNING = "This is your last passkey"
    }
}
