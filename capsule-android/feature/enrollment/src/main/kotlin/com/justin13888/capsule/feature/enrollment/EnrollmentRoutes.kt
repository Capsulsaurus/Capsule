package com.justin13888.capsule.feature.enrollment

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.core.i18n.R
import org.koin.androidx.compose.koinViewModel

/**
 * First-device setup, from the first key to the gate that ends it.
 *
 * One destination for the whole ceremony because it is one ceremony: the steps, the reveal and
 * the type-back are phases of a single "is this account set up" question, and a user who could
 * navigate between them independently could reach the end without passing the gate.
 *
 * @param onFinished the type-back was correct and setup is complete.
 */
@Composable
fun FirstDeviceRoute(
    onFinished: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: FirstDeviceViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val clipboardLabel = stringResource(R.string.android_enrollment_passphrase_clipboard_label)
    val fileName = stringResource(R.string.android_enrollment_passphrase_file_name)
    val passphrase = state.passphrase

    val save =
        rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument(PLAIN_TEXT)) { uri ->
            val text = passphrase?.text ?: return@rememberLauncherForActivityResult
            if (uri != null) context.contentResolver.openOutputStream(uri)?.use { it.write(text.toByteArray()) }
        }

    LaunchedEffect(state.phase) {
        if (state.phase == SetupPhase.Complete) onFinished()
    }

    when (state.phase) {
        SetupPhase.Steps, SetupPhase.Complete ->
            FirstDeviceSetupScreen(state = state, onRetryStep = viewModel::retry, modifier = modifier)

        SetupPhase.Passphrase ->
            passphrase?.let {
                RecoveryPassphraseScreen(
                    passphrase = it,
                    hasCopied = state.copied,
                    onCopy = {
                        context.copyToClipboard(clipboardLabel, it.text)
                        viewModel.passphraseCopied()
                    },
                    onPrint = { save.launch(fileName) },
                    onContinue = viewModel::continueToTypeBack,
                    modifier = modifier,
                )
            }

        SetupPhase.TypeBack ->
            TypeBackGateScreen(
                state = state,
                onAnswerChanged = viewModel::answerChanged,
                onSubmit = viewModel::submitTypeBack,
                onBackToWords = viewModel::backToPassphrase,
                modifier = modifier,
            )
    }
}

/**
 * Choosing which end of the cross-device ceremony this device is.
 *
 * @param onShowCode this device issues the code.
 * @param onScanCode this device reads one.
 */
@Composable
fun EnrollmentStartRoute(
    onShowCode: () -> Unit,
    onScanCode: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: EnrollmentStartViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    EnrollmentStartScreen(
        state = state,
        onConfirmIdentity = viewModel::confirm,
        onShowCode = onShowCode,
        onScanCode = onScanCode,
        modifier = modifier,
    )
}

/**
 * The code-issuing side.
 *
 * @param onCompareSafetyCode both devices are ready to compare safety codes.
 */
@Composable
fun EnrollmentCodeRoute(
    onCompareSafetyCode: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: EnrollmentCodeViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    EnrollmentCodeScreen(
        state = state,
        onRegenerate = viewModel::issue,
        onCompareSafetyCode = onCompareSafetyCode,
        modifier = modifier,
    )
}

/**
 * The code-reading side, over a real camera preview.
 *
 * The permission is asked for from here rather than inside the screen so the screen stays
 * previewable and testable without an activity behind it.
 *
 * @param onCodeAccepted a code was read or typed and accepted.
 */
@Composable
fun EnrollmentScanRoute(
    onCodeAccepted: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: EnrollmentScanViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val request =
        rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            viewModel.cameraPermissionChanged(granted)
        }

    LaunchedEffect(Unit) {
        viewModel.cameraPermissionChanged(context.hasCameraPermission())
    }
    LaunchedEffect(state.acceptedCode) {
        if (state.acceptedCode != null) onCodeAccepted()
    }

    EnrollmentScanScreen(
        state = state,
        onRequestCamera = { request.launch(Manifest.permission.CAMERA) },
        onManualCodeChanged = viewModel::manualCodeChanged,
        onSubmitManualCode = viewModel::submitManualCode,
        modifier = modifier,
    )
}

/**
 * The safety-code comparison, on either device.
 *
 * @param onSettled the ceremony reached an outcome, successful or deferred.
 */
@Composable
fun EnrollmentApproveRoute(
    onSettled: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: SafetyCodeViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.outcome) {
        if (state.outcome != null) onSettled()
    }
    SafetyCodeScreen(
        state = state,
        onAcknowledgementChanged = viewModel::acknowledgementChanged,
        onConfirm = viewModel::confirm,
        onAbort = viewModel::abort,
        onStartOver = viewModel::startOver,
        modifier = modifier,
    )
}

/**
 * How the ceremony ended.
 *
 * @param onDone leave the ceremony.
 */
@Composable
fun EnrollmentCompleteRoute(
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: EnrollmentCompleteViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    EnrollmentCompleteScreen(state = state, onDone = onDone, modifier = modifier)
}

private const val PLAIN_TEXT = "text/plain"

private fun Context.hasCameraPermission(): Boolean =
    ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

/**
 * Puts the passphrase on the clipboard.
 *
 * Only ever from an explicit tap, and the screen says what to do next with it. The platform
 * clipboard is used directly rather than through Compose's wrapper because this is a one-shot
 * side effect with no composition state, and the wrapper's API has churned across releases.
 */
private fun Context.copyToClipboard(
    label: String,
    text: String,
) {
    getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(label, text))
}
