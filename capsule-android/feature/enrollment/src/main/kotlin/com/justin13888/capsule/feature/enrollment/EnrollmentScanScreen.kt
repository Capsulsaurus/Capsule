package com.justin13888.capsule.feature.enrollment

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R

/** Test handle for the camera permission primer. */
const val CAMERA_PRIMER_TAG: String = "enrollment.camera-primer"

/** Test handle for the hand-typed fallback, which is a peer of the camera and not a rescue. */
const val MANUAL_ENTRY_TAG: String = "enrollment.manual-entry"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp
private val ViewfinderCorner = 24.dp

/**
 * Reading the code the other device is showing.
 *
 * The viewfinder is a slot rather than a hard-coded camera so that this screen is composable
 * — and therefore previewable and testable — on a JVM with no camera at all. Production passes
 * the real one; a test passes something inert and still exercises everything around it, which
 * is where all of this screen's behaviour actually lives.
 */
@Suppress("LongParameterList")
@Composable
fun EnrollmentScanScreen(
    state: ScanUiState,
    onRequestCamera: () -> Unit,
    onManualCodeChanged: (String) -> Unit,
    onSubmitManualCode: () -> Unit,
    modifier: Modifier = Modifier,
    viewfinder: @Composable () -> Unit = { CameraViewfinder() },
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_enrollment_scan_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_scan_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (state.hasCameraPermission) {
            Column(
                modifier = Modifier.fillMaxWidth().aspectRatio(1f).clip(RoundedCornerShape(ViewfinderCorner)),
            ) {
                viewfinder()
            }
            if (state.isReading) {
                CapsuleLoadingState(
                    title = stringResource(R.string.android_enrollment_scan_reading),
                    contentDescription = stringResource(R.string.android_enrollment_scan_reading),
                )
            }
        } else {
            CameraPrimer(onRequestCamera)
        }
        state.failure?.let { failure ->
            CapsuleBanner(
                title = stringResource(R.string.android_enrollment_scan_refused_title),
                body = localizedErrorMessage(failure.code),
                severity = CapsuleBannerSeverity.Error,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        ManualEntry(
            state = state,
            onManualCodeChanged = onManualCodeChanged,
            onSubmitManualCode = onSubmitManualCode,
        )
    }
}

@Composable
private fun CameraPrimer(
    onRequestCamera: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_enrollment_scan_permission_title),
        body = stringResource(R.string.android_enrollment_scan_permission_body),
        severity = CapsuleBannerSeverity.Info,
        modifier = modifier.fillMaxWidth().testTag(CAMERA_PRIMER_TAG),
        action = {
            CapsuleButton(
                onClick = onRequestCamera,
                label = stringResource(R.string.android_enrollment_scan_permission_action),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

/**
 * The typed fallback.
 *
 * Always on screen, never behind a link. A user whose camera is refused, broken, or pointed at
 * a device across the room needs this to be the obvious second option rather than a discovery.
 */
@Composable
private fun ManualEntry(
    state: ScanUiState,
    onManualCodeChanged: (String) -> Unit,
    onSubmitManualCode: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleCard(modifier = modifier.fillMaxWidth().testTag(MANUAL_ENTRY_TAG)) {
        Column(
            modifier = Modifier.padding(ScreenPadding),
            verticalArrangement = Arrangement.spacedBy(BlockSpacing),
        ) {
            CapsuleSectionHeader(title = stringResource(R.string.android_enrollment_scan_manual_title))
            EnrollmentTextField(
                value = state.manualCode,
                onValueChange = onManualCodeChanged,
                label = stringResource(R.string.android_enrollment_scan_manual_label),
                isError = state.failure != null,
            )
            CapsuleButton(
                onClick = onSubmitManualCode,
                label = stringResource(R.string.android_enrollment_scan_manual_submit),
                enabled = state.manualCode.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

@CapsulePreviews
@Composable
internal fun ScanPrimerPreview() {
    CapsulePreviewSurface {
        EnrollmentScanScreen(
            state = ScanUiState(),
            onRequestCamera = {},
            onManualCodeChanged = {},
            onSubmitManualCode = {},
        )
    }
}

@CapsulePreviews
@Composable
internal fun ScanReadingPreview() {
    CapsulePreviewSurface {
        EnrollmentScanScreen(
            state = ScanUiState(hasCameraPermission = true, isReading = true),
            onRequestCamera = {},
            onManualCodeChanged = {},
            onSubmitManualCode = {},
        )
    }
}
