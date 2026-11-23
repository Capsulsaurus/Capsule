package com.justin13888.capsule.feature.enrollment

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
import kotlinx.collections.immutable.toImmutableMap
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import kotlin.random.Random

/**
 * The accessibility framework's own opinion of the two screens where getting it wrong costs
 * the most: the passphrase a user has to transcribe, and the safety code they have to read
 * aloud to someone in another room.
 *
 * Run at [RoborazziATFAccessibilityChecker.CheckLevel.Warning] rather than `Error`, which is
 * permissive enough to pass a twelve-dp touch target and would therefore only ever agree.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(application = Application::class, qualifiers = "w412dp-h915dp-xhdpi")
class EnrollmentAccessibilityTest {
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
    fun `the passphrase grid is navigable`() {
        check {
            RecoveryPassphraseScreen(
                passphrase = RecoveryPassphrase.generate(Random(2)),
                hasCopied = false,
                onCopy = {},
                onPrint = {},
                onContinue = {},
            )
        }
    }

    /**
     * Checked with the fields filled in.
     *
     * The empty gate disables its submit button, and a disabled control is exempt from the
     * contrast requirement the framework applies to it — WCAG 1.4.3 says so explicitly — so
     * checking the empty state would be checking an exemption rather than the screen.
     */
    @Test
    fun `the type-back gate is navigable`() {
        val positions = persistentListOf(0, 3, 7)
        check {
            TypeBackGateScreen(
                state =
                    FirstDeviceUiState(
                        phase = SetupPhase.TypeBack,
                        passphrase = RecoveryPassphrase.generate(Random(2)),
                        challenge = TypeBackChallenge(positions),
                        answers = positions.associateWith { "harbor" }.toImmutableMap(),
                    ),
                onAnswerChanged = { _, _ -> },
                onSubmit = {},
                onBackToWords = {},
            )
        }
    }

    @Test
    fun `the safety-code comparison is navigable`() {
        check {
            SafetyCodeScreen(
                state =
                    SafetyCodeUiState(
                        safetyCode = SafetyCode(persistentListOf("4F2A", "91BD", "70C3")),
                        thisDevice = DeviceIdentity("Pixel 8", "4F2A"),
                        otherDevice = DeviceIdentity("Pixel 9 Pro", "70C3"),
                    ),
                onAcknowledgementChanged = {},
                onConfirm = {},
                onAbort = {},
                onStartOver = {},
            )
        }
    }
}
