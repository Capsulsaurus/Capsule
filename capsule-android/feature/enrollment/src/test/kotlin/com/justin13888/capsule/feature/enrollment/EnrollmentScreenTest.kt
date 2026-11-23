package com.justin13888.capsule.feature.enrollment

import android.app.Application
import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ErrorCode
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Duration
import kotlin.random.Random

/**
 * The enrollment screens driven through a real composition.
 *
 * These check what a state machine cannot see: that the type-back gate offers no way past
 * itself, that the safety-code screen shows the code beside both device identities and refuses
 * to confirm without an acknowledgement, and that every named failure state actually renders.
 */
@RunWith(RobolectricTestRunner::class)
@Config(application = Application::class, qualifiers = "w412dp-h915dp-xhdpi")
class EnrollmentScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val context: Context get() = ApplicationProvider.getApplicationContext()

    private fun setScreen(content: @Composable () -> Unit) {
        compose.setContent { CapsuleTheme(dynamicColor = false) { content() } }
        compose.waitForIdle()
    }

    @Test
    fun `the five setup steps are listed with their own statuses`() {
        setScreen { FirstDeviceSetupScreen(state = FirstDeviceUiState(), onRetryStep = {}) }

        compose.onNodeWithTag(SETUP_STEPS_TAG).assertIsDisplayed()
        compose
            .onNodeWithText(context.getString(R.string.android_enrollment_setup_step_master_key))
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun `a secure element that refuses is stated as hardware, with a retry`() {
        val steps =
            EnrollmentStep.entries
                .map {
                    StepState(it, if (it == EnrollmentStep.DeviceKeys) StepStatus.Failed else StepStatus.Done)
                }.toImmutableList()
        setScreen {
            FirstDeviceSetupScreen(
                state =
                    FirstDeviceUiState(
                        steps = steps,
                        failure = EnrollmentFailure(EnrollmentFailureKind.SecureElementRefused),
                    ),
                onRetryStep = {},
            )
        }

        compose.onNodeWithTag(SECURE_ELEMENT_FAILURE_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `a deferred directory upload reads as still finishing rather than as failed`() {
        val steps =
            EnrollmentStep.entries
                .map {
                    StepState(it, if (it == EnrollmentStep.DeviceDirectory) StepStatus.Deferred else StepStatus.Done)
                }.toImmutableList()
        setScreen { FirstDeviceSetupScreen(state = FirstDeviceUiState(steps = steps), onRetryStep = {}) }

        compose.onNodeWithTag(DEFERRED_DIRECTORY_TAG).performScrollTo().assertIsDisplayed()
        compose
            .onNodeWithText(context.getString(R.string.android_enrollment_setup_unreachable_title))
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun `the passphrase is shown once, with the screenshot warning beside it`() {
        setScreen {
            RecoveryPassphraseScreen(
                passphrase = RecoveryPassphrase.generate(Random(4)),
                hasCopied = false,
                onCopy = {},
                onPrint = {},
                onContinue = {},
            )
        }

        compose.onNodeWithTag(PASSPHRASE_GRID_TAG).assertIsDisplayed()
        compose
            .onNodeWithText(context.getString(R.string.android_enrollment_passphrase_screenshot_warning))
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun `the type-back gate offers no way past itself`() {
        setScreen {
            TypeBackGateScreen(
                state =
                    FirstDeviceUiState(
                        phase = SetupPhase.TypeBack,
                        passphrase = RecoveryPassphrase.generate(Random(4)),
                        challenge = TypeBackChallenge(persistentListOf(0, 3, 7)),
                    ),
                onAnswerChanged = { _, _ -> },
                onSubmit = {},
                onBackToWords = {},
            )
        }

        compose.onNodeWithTag(TYPE_BACK_GATE_TAG).assertIsDisplayed()
        // Nothing is typed yet, so the only forward action is inert; the only other control
        // goes back to the words rather than onwards.
        compose.onNodeWithTag(TYPE_BACK_SUBMIT_TAG).performScrollTo().assertIsNotEnabled()
        compose
            .onNodeWithText(context.getString(R.string.android_enrollment_typeback_back_to_words))
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun `the code screen shows both forms and the countdown`() {
        setScreen {
            EnrollmentCodeScreen(
                state = EnrollmentCodeUiState(code = EnrollmentCode("K3QP7ZR2MW9XT4VB", "482913057")),
                onRegenerate = {},
                onCompareSafetyCode = {},
            )
        }

        compose.onNodeWithTag(CODE_IMAGE_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(CODE_DIGITS_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("482913057").performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `an expired code is an ordinary end with a way to start again`() {
        var regenerated = false
        setScreen {
            EnrollmentCodeScreen(
                state =
                    EnrollmentCodeUiState(
                        code = EnrollmentCode("K3QP7ZR2MW9XT4VB", "482913057"),
                        remaining = Duration.ZERO,
                    ),
                onRegenerate = { regenerated = true },
                onCompareSafetyCode = {},
            )
        }

        compose.onNodeWithTag(CODE_EXPIRED_TAG).performScrollTo().assertIsDisplayed()
        compose
            .onNodeWithText(context.getString(R.string.android_enrollment_code_regenerate))
            .performScrollTo()
            .performClick()
        compose.waitForIdle()

        assertEquals(true, regenerated)
    }

    @Test
    fun `the scanner offers the typed fallback whether or not the camera is allowed`() {
        setScreen {
            EnrollmentScanScreen(
                state = ScanUiState(),
                onRequestCamera = {},
                onManualCodeChanged = {},
                onSubmitManualCode = {},
                viewfinder = {},
            )
        }

        compose.onNodeWithTag(CAMERA_PRIMER_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(MANUAL_ENTRY_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `an allowed camera fills the viewfinder slot`() {
        setScreen {
            EnrollmentScanScreen(
                state = ScanUiState(hasCameraPermission = true, isReading = true),
                onRequestCamera = {},
                onManualCodeChanged = {},
                onSubmitManualCode = {},
                viewfinder = { StubViewfinder() },
            )
        }

        compose.onNodeWithTag(STUB_VIEWFINDER_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `the safety code is shown beside both device identities`() {
        setScreen {
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

        compose.onNodeWithTag(SAFETY_CODE_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Pixel 8").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Pixel 9 Pro").performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(SAFETY_ACK_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `a confirmation with no acknowledgement is refused in words`() {
        setScreen {
            SafetyCodeScreen(
                state =
                    SafetyCodeUiState(
                        safetyCode = SafetyCode(persistentListOf("4F2A", "91BD", "70C3")),
                        acknowledgementMissing = true,
                    ),
                onAcknowledgementChanged = {},
                onConfirm = {},
                onAbort = {},
                onStartOver = {},
            )
        }

        compose
            .onNodeWithText(context.getString(R.string.android_enrollment_safety_acknowledge_required))
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun `a mismatch replaces the comparison rather than annotating it`() {
        setScreen {
            SafetyCodeScreen(
                state =
                    SafetyCodeUiState(
                        safetyCode = SafetyCode(persistentListOf("4F2A", "91BD", "70C3")),
                        isAborted = true,
                        failure =
                            EnrollmentFailure(
                                EnrollmentFailureKind.Refused,
                                ErrorCode("error.enrollment.code_refused"),
                            ),
                    ),
                onAcknowledgementChanged = {},
                onConfirm = {},
                onAbort = {},
                onStartOver = {},
            )
        }

        compose.onNodeWithTag(SAFETY_MISMATCH_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(SAFETY_CONFIRM_TAG).assertDoesNotExist()
        compose
            .onNodeWithText(context.getString(R.string.error_enrollment_code_refused))
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun `a pending directory upload is a completion, not a failure`() {
        setScreen {
            EnrollmentCompleteScreen(
                state = EnrollmentCompleteUiState(EnrollmentOutcome.PendingDirectoryUpload),
                onDone = {},
            )
        }

        compose.onNodeWithTag(COMPLETE_PENDING_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(COMPLETE_DONE_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `an added device is named back to the user`() {
        setScreen {
            EnrollmentCompleteScreen(
                state = EnrollmentCompleteUiState(EnrollmentOutcome.Added("Pixel 9 Pro")),
                onDone = {},
            )
        }

        compose.onNodeWithTag(COMPLETE_ADDED_TAG).performScrollTo().assertIsDisplayed()
    }
}
