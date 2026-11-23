package com.justin13888.capsule.feature.enrollment

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R

/** Test handle for the added-device outcome. */
const val COMPLETE_ADDED_TAG: String = "enrollment.complete-added"

/** Test handle for the outcome where the keys are valid and the upload is still pending. */
const val COMPLETE_PENDING_TAG: String = "enrollment.complete-pending"

/** Test handle for the action that leaves the ceremony. */
const val COMPLETE_DONE_TAG: String = "enrollment.complete-done"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp

/**
 * How the ceremony ended.
 *
 * Two endings, both successful. A directory upload that has not landed is not a partial
 * enrollment: the new device's keys are generated, signed and valid, and the only thing
 * missing is other devices knowing about it — which the client retries on its own.
 */
@Composable
fun EnrollmentCompleteScreen(
    state: EnrollmentCompleteUiState,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        when (val outcome = state.outcome) {
            is EnrollmentOutcome.Added ->
                CapsuleEmptyState(
                    title = stringResource(R.string.android_enrollment_complete_title),
                    body =
                        stringResource(R.string.android_enrollment_complete_body)
                            .withArguments("device" to outcome.deviceName),
                    icon = Icons.Filled.Devices,
                    modifier = Modifier.fillMaxWidth().testTag(COMPLETE_ADDED_TAG),
                )

            EnrollmentOutcome.PendingDirectoryUpload, null ->
                CapsuleBanner(
                    title = stringResource(R.string.android_enrollment_complete_pending_title),
                    body = stringResource(R.string.android_enrollment_complete_pending_body),
                    severity = CapsuleBannerSeverity.Degraded,
                    modifier = Modifier.fillMaxWidth().testTag(COMPLETE_PENDING_TAG),
                )
        }
        CapsuleButton(
            onClick = onDone,
            label = stringResource(R.string.android_enrollment_complete_done),
            modifier = Modifier.fillMaxWidth().testTag(COMPLETE_DONE_TAG),
        )
    }
}

@CapsulePreviews
@Composable
internal fun CompleteAddedPreview() {
    CapsulePreviewSurface {
        EnrollmentCompleteScreen(
            state = EnrollmentCompleteUiState(EnrollmentOutcome.Added("Pixel 9 Pro")),
            onDone = {},
        )
    }
}

@CapsulePreviews
@Composable
internal fun CompletePendingPreview() {
    CapsulePreviewSurface {
        EnrollmentCompleteScreen(
            state = EnrollmentCompleteUiState(EnrollmentOutcome.PendingDirectoryUpload),
            onDone = {},
        )
    }
}
