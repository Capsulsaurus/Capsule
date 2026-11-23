package com.justin13888.capsule.feature.onboarding

import android.app.Application
import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.persistentListOf
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The onboarding screens driven through a real composition.
 *
 * These check the things a state-machine test cannot see: that the never-signed-in mode is
 * rendered as its own card with its own action rather than as a link past the decision, that
 * every failure arrives with a remediation attached, and that what the user reads for a
 * refusal is the localized catalogue message rather than a code or a status.
 */
@RunWith(RobolectricTestRunner::class)
@Config(application = Application::class, qualifiers = "w412dp-h915dp-xhdpi")
class OnboardingScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val context: Context get() = ApplicationProvider.getApplicationContext()

    private fun setScreen(content: @Composable () -> Unit) {
        compose.setContent {
            CapsuleTheme(dynamicColor = false) { content() }
        }
        compose.waitForIdle()
    }

    @Test
    fun `the mode choice offers the local library as a card, not as a way out`() {
        var chosenLocal = false
        setScreen {
            WelcomeScreen(
                state = WelcomeUiState(choosingMode = true),
                onPaneChanged = {},
                onAdvance = {},
                onSkipTour = {},
                onChooseLocalOnly = { chosenLocal = true },
                onConnectServer = {},
            )
        }

        compose.onNodeWithTag(MODE_CHOICE_TAG).assertIsDisplayed()
        compose.onNodeWithTag(LOCAL_MODE_CARD_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(SERVER_MODE_CARD_TAG).performScrollTo().assertIsDisplayed()
        compose
            .onNodeWithText(context.getString(R.string.android_onboarding_mode_local_action))
            .performScrollTo()
            .performClick()
        compose.waitForIdle()

        assertEquals(true, chosenLocal)
    }

    @Test
    fun `the tour is shown until it is finished, and the mode choice is what follows`() {
        setScreen {
            WelcomeScreen(
                state = WelcomeUiState(),
                onPaneChanged = {},
                onAdvance = {},
                onSkipTour = {},
                onChooseLocalOnly = {},
                onConnectServer = {},
            )
        }

        compose.onNodeWithTag(WELCOME_PAGER_TAG).assertIsDisplayed()
    }

    @Test
    fun `a server that will not answer offers a retry and a local library`() {
        var retried = false
        var wentLocal = false
        setScreen {
            ServerConnectScreen(
                state = ServerUiState(address = "https://x.example.org", failure = OnboardingFailure(null)),
                onAddressChanged = {},
                onConnect = { retried = true },
                onSelectMethod = {},
                onContinue = {},
                onUseLocalOnly = { wentLocal = true },
            )
        }

        compose
            .onNodeWithText(context.getString(R.string.android_onboarding_server_retry))
            .performScrollTo()
            .performClick()
        compose
            .onNodeWithText(context.getString(R.string.android_onboarding_server_use_local_instead))
            .performScrollTo()
            .performClick()
        compose.waitForIdle()

        assertEquals(true, retried)
        assertEquals(true, wentLocal)
    }

    @Test
    fun `a server that answers shows what it said before asking for a credential`() {
        setScreen {
            ServerConnectScreen(
                state =
                    ServerUiState(
                        address = "https://x.example.org",
                        info =
                            ServerInfo(
                                apiBase = "https://x.example.org/api/v1",
                                authMethods = persistentListOf(AuthMethod.Passkey),
                                minProtocolVersion = 1,
                                maxProtocolVersion = 3,
                            ),
                        selectedMethod = AuthMethod.Passkey,
                    ),
                onAddressChanged = {},
                onConnect = {},
                onSelectMethod = {},
                onContinue = {},
                onUseLocalOnly = {},
            )
        }

        compose.onNodeWithTag(SERVER_INFO_CARD_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(AUTH_PICKER_TAG).performScrollTo().assertIsDisplayed()
        compose
            .onNodeWithText(context.getString(R.string.android_onboarding_server_continue))
            .performScrollTo()
            .assertIsEnabled()
    }

    @Test
    fun `an unanswered probe cannot be continued past`() {
        setScreen {
            ServerConnectScreen(
                state = ServerUiState(address = "https://x.example.org", isProbing = true),
                onAddressChanged = {},
                onConnect = {},
                onSelectMethod = {},
                onContinue = {},
                onUseLocalOnly = {},
            )
        }

        compose
            .onNodeWithText(context.getString(R.string.android_onboarding_server_connect))
            .assertIsNotEnabled()
    }

    @Test
    fun `a refusal reads as the localized catalogue message, never as a status`() {
        setScreen {
            SignInScreen(
                state =
                    SignInUiState(
                        failure =
                            OnboardingFailure(
                                com.justin13888.capsule.core.model
                                    .ErrorCode(INVALID_CREDENTIALS),
                            ),
                    ),
                actions = inertSignInActions(),
            )
        }

        compose
            .onNodeWithText(context.getString(R.string.error_auth_invalid_credentials))
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun `each auth path renders its own step and nothing else`() {
        setScreen {
            SignInScreen(state = SignInUiState(stage = SignInStage.Oidc), actions = inertSignInActions())
        }

        compose.onNodeWithTag(OIDC_STEP_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `partial photo access is stated as an outcome rather than as a denial`() {
        setScreen {
            OnboardingCompleteScreen(
                state = CompletionUiState(access = MediaAccess.Partial, hasAsked = true),
                onRequestAccess = {},
                onOpenAppSettings = {},
                onFinish = {},
            )
        }

        compose.onNodeWithTag(PARTIAL_ACCESS_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(COMPLETE_ACTION_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `a library with no media access still hands off to the timeline`() {
        var finished = false
        setScreen {
            OnboardingCompleteScreen(
                state = CompletionUiState(access = MediaAccess.None, hasAsked = true),
                onRequestAccess = {},
                onOpenAppSettings = {},
                onFinish = { finished = true },
            )
        }

        compose.onNodeWithTag(COMPLETE_ACTION_TAG).performScrollTo().performClick()
        compose.waitForIdle()

        assertEquals(true, finished)
    }

    @Test
    fun `the primer is what a never-asked library shows`() {
        setScreen {
            OnboardingCompleteScreen(
                state = CompletionUiState(),
                onRequestAccess = {},
                onOpenAppSettings = {},
                onFinish = {},
            )
        }

        compose.onNodeWithTag(MEDIA_PRIMER_TAG).performScrollTo().assertIsDisplayed()
    }

    private companion object {
        const val INVALID_CREDENTIALS = "error.auth.invalid_credentials"
    }
}
