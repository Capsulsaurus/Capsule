package com.justin13888.capsule.feature.onboarding

import android.app.Application
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import kotlinx.collections.immutable.persistentListOf
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * The accessibility framework's own opinion of these screens.
 *
 * Run at [RoborazziATFAccessibilityChecker.CheckLevel.Warning] rather than `Error`: the error
 * level is permissive enough to pass a twelve-dp touch target, which would make this a test
 * that only ever agrees with whatever was written.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(application = Application::class, qualifiers = "w412dp-h915dp-xhdpi")
class OnboardingAccessibilityTest {
    @get:Rule
    val compose = createComposeRule()

    private val options =
        RoborazziATFAccessibilityCheckOptions(
            checker = RoborazziATFAccessibilityChecker(),
            failureLevel = RoborazziATFAccessibilityChecker.CheckLevel.Warning,
        )

    private fun check(content: @Composable () -> Unit) {
        compose.setContent { CapsuleTheme(dynamicColor = false) { content() } }
        compose.waitForIdle()
        compose.onRoot().checkRoboAccessibility(options)
    }

    @Test
    fun `the mode choice is navigable`() {
        check {
            WelcomeScreen(
                state = WelcomeUiState(choosingMode = true),
                onPaneChanged = {},
                onAdvance = {},
                onSkipTour = {},
                onChooseLocalOnly = {},
                onConnectServer = {},
            )
        }
    }

    @Test
    fun `the server card and its picker are navigable`() {
        check {
            ServerConnectScreen(
                state =
                    ServerUiState(
                        address = "https://x.example.org",
                        info =
                            ServerInfo(
                                apiBase = "https://x.example.org/api/v1",
                                authMethods = persistentListOf(AuthMethod.Passkey, AuthMethod.Oidc),
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
    }
}
