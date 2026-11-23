package com.justin13888.capsule.feature.enrollment

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleCheckbox
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.persistentListOf

/** Test handle for the chunked safety code. */
const val SAFETY_CODE_TAG: String = "enrollment.safety-code"

/** Test handle for the explicit match-and-identity acknowledgement. */
const val SAFETY_ACK_TAG: String = "enrollment.safety-ack"

/** Test handle for the confirm action, which does nothing until the acknowledgement is given. */
const val SAFETY_CONFIRM_TAG: String = "enrollment.safety-confirm"

/** Test handle for the abort path. */
const val SAFETY_MISMATCH_TAG: String = "enrollment.safety-mismatch"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp

/**
 * The comparison the whole ceremony rests on.
 *
 * Three things are on this screen because the doc says the code alone is not enough: the code,
 * and each end's model and key fingerprint. A relay that swapped in a different device would
 * produce a matching code against the wrong hardware, so the acknowledgement covers both — and
 * it is unticked by default, which is what makes "they do not match" the shape of a decision
 * rather than of a missed default.
 */
@Suppress("LongParameterList")
@Composable
fun SafetyCodeScreen(
    state: SafetyCodeUiState,
    onAcknowledgementChanged: (Boolean) -> Unit,
    onConfirm: () -> Unit,
    onAbort: () -> Unit,
    onStartOver: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (state.isAborted) {
        Mismatch(failure = state.failure, onStartOver = onStartOver, modifier = modifier)
        return
    }
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_enrollment_safety_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_safety_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        state.safetyCode?.let { ChunkedCode(it) }
        state.thisDevice?.let { IdentityCard(R.string.android_enrollment_safety_this_device, it) }
        state.otherDevice?.let { IdentityCard(R.string.android_enrollment_safety_other_device, it) }
        Acknowledgement(state = state, onAcknowledgementChanged = onAcknowledgementChanged)
        if (state.acknowledgementMissing) {
            CapsuleBanner(
                title = stringResource(R.string.android_enrollment_safety_acknowledge_required),
                severity = CapsuleBannerSeverity.Warning,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        if (state.isConfirming) {
            CapsuleLoadingState(
                title = stringResource(R.string.android_enrollment_safety_title),
                contentDescription = stringResource(R.string.android_enrollment_safety_title),
            )
        }
        CapsuleButton(
            onClick = onConfirm,
            label = stringResource(R.string.android_enrollment_safety_confirm),
            enabled = state.canConfirm,
            modifier = Modifier.fillMaxWidth().testTag(SAFETY_CONFIRM_TAG),
        )
        CapsuleButton(
            onClick = onAbort,
            label = stringResource(R.string.android_enrollment_safety_mismatch_action),
            emphasis = CapsuleButtonEmphasis.Destructive,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/**
 * The code, in fixed-length groups.
 *
 * Announced as one string rather than as separate groups, so a screen-reader user hears the
 * same thing the other device's user is reading aloud.
 */
@Composable
private fun ChunkedCode(
    code: SafetyCode,
    modifier: Modifier = Modifier,
) {
    val announcement =
        stringResource(R.string.android_enrollment_safety_code_description).withArguments("code" to code.text)
    Text(
        text = code.text,
        style = MaterialTheme.typography.displaySmallEmphasized,
        textAlign = TextAlign.Center,
        modifier = modifier.fillMaxWidth().testTag(SAFETY_CODE_TAG).capsuleDescription(announcement),
    )
}

@Composable
private fun IdentityCard(
    labelRes: Int,
    identity: DeviceIdentity,
    modifier: Modifier = Modifier,
) {
    CapsuleCard(modifier = modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(ScreenPadding),
            verticalArrangement = Arrangement.spacedBy(BlockSpacing / 4),
        ) {
            Text(
                text = stringResource(labelRes),
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Text(text = identity.model, style = MaterialTheme.typography.titleMediumEmphasized)
            Text(
                text =
                    stringResource(R.string.android_enrollment_safety_fingerprint)
                        .withArguments("fingerprint" to identity.keyFingerprint),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

@Composable
private fun Acknowledgement(
    state: SafetyCodeUiState,
    onAcknowledgementChanged: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    val label = stringResource(R.string.android_enrollment_safety_acknowledge)
    Row(
        modifier = modifier.fillMaxWidth().testTag(SAFETY_ACK_TAG),
        horizontalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        CapsuleCheckbox(
            checked = state.isAcknowledged,
            onCheckedChange = onAcknowledgementChanged,
            contentDescription = label,
        )
        Text(text = label, style = MaterialTheme.typography.bodyMedium)
    }
}

/**
 * The abort screen.
 *
 * Reached both ways it can be reached — the user saying the codes differ, and the server
 * refusing the code — because they mean the same thing to the person holding the phone:
 * something was between the two devices, and nothing was handed over.
 */
@Composable
private fun Mismatch(
    failure: EnrollmentFailure?,
    onStartOver: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        CapsuleErrorState(
            title = stringResource(R.string.android_enrollment_safety_mismatch_title),
            body = stringResource(R.string.android_enrollment_safety_mismatch_body),
            modifier = Modifier.fillMaxWidth().testTag(SAFETY_MISMATCH_TAG),
            action = {
                CapsuleButton(
                    onClick = onStartOver,
                    label = stringResource(R.string.android_enrollment_safety_mismatch_restart),
                )
            },
        )
        failure?.let {
            CapsuleBanner(
                title = stringResource(R.string.android_enrollment_error_title),
                body = localizedErrorMessage(it.code),
                severity = CapsuleBannerSeverity.Error,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

private val SAMPLE_CODE = SafetyCode(persistentListOf("4F2A", "91BD", "70C3"))

@CapsulePreviews
@Composable
internal fun SafetyCodePreview() {
    CapsulePreviewSurface {
        SafetyCodeScreen(
            state =
                SafetyCodeUiState(
                    safetyCode = SAMPLE_CODE,
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

@CapsulePreviews
@Composable
internal fun SafetyMismatchPreview() {
    CapsulePreviewSurface {
        SafetyCodeScreen(
            state = SafetyCodeUiState(safetyCode = SAMPLE_CODE, isAborted = true),
            onAcknowledgementChanged = {},
            onConfirm = {},
            onAbort = {},
            onStartOver = {},
        )
    }
}
