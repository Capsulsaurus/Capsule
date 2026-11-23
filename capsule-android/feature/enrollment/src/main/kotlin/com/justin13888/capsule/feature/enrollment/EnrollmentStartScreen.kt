package com.justin13888.capsule.feature.enrollment

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
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
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R

/** Test handle for the fresh-authorization gate in front of issuing a code. */
const val LOCAL_AUTH_GATE_TAG: String = "enrollment.local-auth"

/** Test handle for the two sides of the ceremony. */
const val CEREMONY_SIDES_TAG: String = "enrollment.sides"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp

/**
 * Choosing which end of the ceremony this device is.
 *
 * The two sides are only offered once physical control of this device has been proven. That
 * ordering is the point: a session token that was exfiltrated from somewhere else must not be
 * able to reach the code, so the gate comes first and the choice is not rendered behind it.
 */
@Suppress("LongParameterList")
@Composable
fun EnrollmentStartScreen(
    state: EnrollmentStartUiState,
    onConfirmIdentity: () -> Unit,
    onShowCode: () -> Unit,
    onScanCode: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_enrollment_start_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_start_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (state.isAuthorized) {
            CeremonySides(onShowCode = onShowCode, onScanCode = onScanCode)
        } else {
            LocalAuthGate(state = state, onConfirmIdentity = onConfirmIdentity)
        }
    }
}

@Composable
private fun LocalAuthGate(
    state: EnrollmentStartUiState,
    onConfirmIdentity: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxWidth().testTag(LOCAL_AUTH_GATE_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_enrollment_start_local_auth_title),
            style = MaterialTheme.typography.titleLargeEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_start_local_auth_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (state.wasDeclined) {
            CapsuleBanner(
                title = stringResource(R.string.android_shell_gate_failed),
                severity = CapsuleBannerSeverity.Error,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        if (state.isConfirming) {
            CapsuleLoadingState(
                title = stringResource(R.string.android_shell_gate_prompt_title),
                contentDescription = stringResource(R.string.android_shell_gate_prompt_title),
            )
        }
        CapsuleButton(
            onClick = onConfirmIdentity,
            label = stringResource(R.string.android_enrollment_start_local_auth_action),
            enabled = !state.isConfirming,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@Composable
private fun CeremonySides(
    onShowCode: () -> Unit,
    onScanCode: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxWidth().testTag(CEREMONY_SIDES_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        SideCard(
            titleRes = R.string.android_enrollment_start_show_code,
            bodyRes = R.string.android_enrollment_start_show_code_body,
            onClick = onShowCode,
            emphasis = CapsuleButtonEmphasis.Primary,
        )
        SideCard(
            titleRes = R.string.android_enrollment_start_scan_code,
            bodyRes = R.string.android_enrollment_start_scan_code_body,
            onClick = onScanCode,
            emphasis = CapsuleButtonEmphasis.Secondary,
        )
    }
}

@Suppress("LongParameterList")
@Composable
private fun SideCard(
    titleRes: Int,
    bodyRes: Int,
    onClick: () -> Unit,
    emphasis: CapsuleButtonEmphasis,
    modifier: Modifier = Modifier,
) {
    CapsuleCard(modifier = modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(ScreenPadding),
            verticalArrangement = Arrangement.spacedBy(BlockSpacing),
        ) {
            Text(text = stringResource(titleRes), style = MaterialTheme.typography.titleLargeEmphasized)
            Text(
                text = stringResource(bodyRes),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            CapsuleButton(
                onClick = onClick,
                label = stringResource(titleRes),
                emphasis = emphasis,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

@CapsulePreviews
@Composable
internal fun StartGatePreview() {
    CapsulePreviewSurface {
        EnrollmentStartScreen(
            state = EnrollmentStartUiState(),
            onConfirmIdentity = {},
            onShowCode = {},
            onScanCode = {},
        )
    }
}

@CapsulePreviews
@Composable
internal fun StartSidesPreview() {
    CapsulePreviewSurface {
        EnrollmentStartScreen(
            state = EnrollmentStartUiState(isAuthorized = true),
            onConfirmIdentity = {},
            onShowCode = {},
            onScanCode = {},
        )
    }
}
