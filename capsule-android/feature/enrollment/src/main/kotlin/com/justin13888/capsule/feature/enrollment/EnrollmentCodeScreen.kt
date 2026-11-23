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
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R
import java.time.Duration

/** Test handle for the square code. */
const val CODE_IMAGE_TAG: String = "enrollment.code-image"

/** Test handle for the friendly numeric fallback. */
const val CODE_DIGITS_TAG: String = "enrollment.code-digits"

/** Test handle for the expired state, which is an ordinary end and not an error. */
const val CODE_EXPIRED_TAG: String = "enrollment.code-expired"

/** Test handle for the step onwards to the safety-code comparison. */
const val CODE_COMPARE_TAG: String = "enrollment.code-compare"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp

/**
 * The code the already-trusted device shows.
 *
 * Both forms are offered together rather than one behind a "can't scan?" link, because the
 * fallback is not a degraded path — it is the one that works across a room, down a phone line,
 * and on a device whose camera is broken. The screen says why it is shorter, so nobody has to
 * guess whether the short one is the weak one.
 */
@Composable
fun EnrollmentCodeScreen(
    state: EnrollmentCodeUiState,
    onRegenerate: () -> Unit,
    onCompareSafetyCode: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_enrollment_code_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_code_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        when {
            state.isIssuing -> Issuing()
            state.failure != null -> IssueFailed(state.failure, onRegenerate)
            state.isExpired -> Expired(onRegenerate)
            state.code != null -> LiveCode(state, state.code, onRegenerate, onCompareSafetyCode)
        }
    }
}

@Composable
private fun Issuing(modifier: Modifier = Modifier) {
    CapsuleLoadingState(
        modifier = modifier.fillMaxWidth(),
        title = stringResource(R.string.android_enrollment_code_waiting),
        contentDescription = stringResource(R.string.android_enrollment_code_waiting),
    )
}

@Composable
private fun IssueFailed(
    failure: EnrollmentFailure,
    onRegenerate: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_enrollment_error_title),
        body = localizedErrorMessage(failure.code),
        modifier = modifier.fillMaxWidth(),
        action = {
            CapsuleButton(
                onClick = onRegenerate,
                label = stringResource(R.string.android_enrollment_code_regenerate),
            )
        },
    )
}

/**
 * The ten-minute window having closed.
 *
 * Deliberately calm: nothing was persisted and nothing leaked, which is exactly what the doc
 * says happens when a user walks away mid-ceremony. The only action is to issue another one.
 */
@Composable
private fun Expired(
    onRegenerate: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_enrollment_code_expired_title),
        body = stringResource(R.string.android_enrollment_code_expired_body),
        severity = CapsuleBannerSeverity.Warning,
        modifier = modifier.fillMaxWidth().testTag(CODE_EXPIRED_TAG),
        action = {
            CapsuleButton(
                onClick = onRegenerate,
                label = stringResource(R.string.android_enrollment_code_regenerate),
            )
        },
    )
}

@Suppress("LongParameterList")
@Composable
private fun LiveCode(
    state: EnrollmentCodeUiState,
    code: EnrollmentCode,
    onRegenerate: () -> Unit,
    onCompareSafetyCode: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(BlockSpacing)) {
        EnrollmentCodeImage(
            payload = code.payload,
            contentDescription = stringResource(R.string.android_enrollment_code_qr_description),
            modifier = Modifier.fillMaxWidth().testTag(CODE_IMAGE_TAG),
        )
        Text(
            text =
                stringResource(R.string.android_enrollment_code_expires_in)
                    .withArguments("remaining" to state.remaining.asCountdown()),
            style = MaterialTheme.typography.titleMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_code_single_use),
            style = MaterialTheme.typography.labelLarge,
        )
        CapsuleCard(modifier = Modifier.fillMaxWidth().testTag(CODE_DIGITS_TAG)) {
            CapsuleSectionHeader(title = stringResource(R.string.android_enrollment_code_numeric_label))
            Text(
                text = code.friendlyDigits,
                style = MaterialTheme.typography.displaySmallEmphasized,
                modifier = Modifier.padding(horizontal = ScreenPadding),
            )
            Text(
                text = stringResource(R.string.android_enrollment_code_numeric_body),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(ScreenPadding),
            )
        }
        CapsuleButton(
            onClick = onCompareSafetyCode,
            label = stringResource(R.string.android_enrollment_code_compare),
            modifier = Modifier.fillMaxWidth().testTag(CODE_COMPARE_TAG),
        )
        CapsuleButton(
            onClick = onRegenerate,
            label = stringResource(R.string.android_enrollment_code_regenerate),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@CapsulePreviews
@Composable
internal fun LiveCodePreview() {
    CapsulePreviewSurface {
        EnrollmentCodeScreen(
            state = EnrollmentCodeUiState(code = EnrollmentCode("K3QP7ZR2MW9XT4VB", "482913057")),
            onRegenerate = {},
            onCompareSafetyCode = {},
        )
    }
}

@CapsulePreviews
@Composable
internal fun ExpiredCodePreview() {
    CapsulePreviewSurface {
        EnrollmentCodeScreen(
            state =
                EnrollmentCodeUiState(
                    code = EnrollmentCode("K3QP7ZR2MW9XT4VB", "482913057"),
                    remaining = Duration.ZERO,
                ),
            onRegenerate = {},
            onCompareSafetyCode = {},
        )
    }
}
