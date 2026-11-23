package com.justin13888.capsule.feature.enrollment

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.ErrorOutline
import androidx.compose.material.icons.filled.HourglassEmpty
import androidx.compose.material.icons.filled.WarningAmber
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleProgress
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R

/** Test handle for the numbered setup steps. */
const val SETUP_STEPS_TAG: String = "enrollment.setup-steps"

/** Test handle for the secure-element refusal, which is the one hard stop in the ceremony. */
const val SECURE_ELEMENT_FAILURE_TAG: String = "enrollment.secure-element-failed"

/** Test handle for the deferred-directory notice, which is an outcome and not a failure. */
const val DEFERRED_DIRECTORY_TAG: String = "enrollment.directory-deferred"

/** Test handle for the deferred default album. */
const val DEFERRED_ALBUM_TAG: String = "enrollment.album-deferred"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp

/**
 * The five steps that bring an account into existence.
 *
 * Each step reports on its own, because they fail differently: a secure element that refuses
 * stops everything, while a directory upload that cannot reach the server stops nothing at all
 * — the keys are already valid, and the doc says so.
 */
@Composable
fun FirstDeviceSetupScreen(
    state: FirstDeviceUiState,
    onRetryStep: (EnrollmentStep) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_enrollment_setup_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_setup_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        CapsuleProgress(
            progress = { state.progress },
            modifier = Modifier.fillMaxWidth(),
            contentDescription = stringResource(R.string.android_enrollment_setup_title),
        )
        Column(modifier = Modifier.fillMaxWidth().testTag(SETUP_STEPS_TAG)) {
            state.steps.forEach { step -> StepRow(step, onRetryStep) }
        }
        if (state.isBlocked) {
            SecureElementRefused(onRetry = { onRetryStep(EnrollmentStep.DeviceKeys) })
        }
        state.deferred.forEach { DeferredNotice(it.step) }
    }
}

@Composable
private fun StepRow(
    step: StepState,
    onRetryStep: (EnrollmentStep) -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleListItem(
        headline = stringResource(step.step.labelRes()),
        supporting = stringResource(step.status.labelRes()),
        modifier = modifier.fillMaxWidth(),
        leading = {
            Icon(imageVector = step.status.icon(), contentDescription = null, tint = step.status.tint())
        },
        trailing = {
            if (step.status == StepStatus.Failed) {
                CapsuleButton(
                    onClick = { onRetryStep(step.step) },
                    label = stringResource(R.string.android_enrollment_setup_retry_step),
                )
            }
        },
    )
}

/**
 * The one refusal that stops setup.
 *
 * Stated as hardware rather than as an app error, because the remedy is different: there is no
 * software fallback that would produce a non-exportable key, so "try again, and if it keeps
 * refusing this hardware cannot hold a Capsule key" is the honest instruction.
 */
@Composable
private fun SecureElementRefused(
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_enrollment_setup_secure_element_title),
        body = stringResource(R.string.android_enrollment_setup_secure_element_body),
        modifier = modifier.fillMaxWidth().testTag(SECURE_ELEMENT_FAILURE_TAG),
        action = {
            CapsuleButton(onClick = onRetry, label = stringResource(R.string.android_enrollment_retry))
        },
    )
}

/**
 * A step that did not finish and does not need to.
 *
 * Rendered as degraded rather than as an error on purpose. Both of these recover on their own
 * — the upload retries with backoff, and the default album's identifier is derivable from the
 * master key — so an error tone would ask the user to act on something that is already handled.
 */
@Composable
private fun DeferredNotice(
    step: EnrollmentStep,
    modifier: Modifier = Modifier,
) {
    val isDirectory = step == EnrollmentStep.DeviceDirectory
    CapsuleBanner(
        title =
            stringResource(
                if (isDirectory) {
                    R.string.android_enrollment_setup_unreachable_title
                } else {
                    R.string.android_enrollment_setup_default_album_title
                },
            ),
        body =
            stringResource(
                if (isDirectory) {
                    R.string.android_enrollment_setup_unreachable_body
                } else {
                    R.string.android_enrollment_setup_default_album_body
                },
            ),
        severity = CapsuleBannerSeverity.Degraded,
        modifier =
            modifier
                .fillMaxWidth()
                .testTag(if (isDirectory) DEFERRED_DIRECTORY_TAG else DEFERRED_ALBUM_TAG),
    )
}

private fun StepStatus.icon(): ImageVector =
    when (this) {
        StepStatus.Pending, StepStatus.Running -> Icons.Filled.HourglassEmpty
        StepStatus.Done -> Icons.Filled.CheckCircle
        StepStatus.Failed -> Icons.Filled.ErrorOutline
        StepStatus.Deferred -> Icons.Filled.WarningAmber
    }

@Composable
private fun StepStatus.tint() =
    when (this) {
        StepStatus.Pending, StepStatus.Running -> MaterialTheme.colorScheme.onSurfaceVariant
        StepStatus.Done -> CapsuleTheme.colors.success.color
        StepStatus.Failed -> MaterialTheme.colorScheme.error
        StepStatus.Deferred -> CapsuleTheme.colors.degraded.color
    }

@CapsulePreviews
@Composable
internal fun SetupStepsPreview() {
    CapsulePreviewSurface {
        FirstDeviceSetupScreen(state = FirstDeviceUiState(), onRetryStep = {})
    }
}
