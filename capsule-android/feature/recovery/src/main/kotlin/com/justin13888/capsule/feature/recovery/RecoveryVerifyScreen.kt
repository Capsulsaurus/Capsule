package com.justin13888.capsule.feature.recovery

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleToggleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/** Test handle for the whole verification surface. */
const val RECOVERY_VERIFY_TAG: String = "recovery.verify"

/** Test handle for the passphrase field. */
const val RECOVERY_VERIFY_FIELD_TAG: String = "recovery.verify.field"

/** Test handle for the success state. */
const val RECOVERY_VERIFY_OK_TAG: String = "recovery.verify.verified"

/** Test handle for the mismatch state, which is emphatically not an error state. */
const val RECOVERY_VERIFY_MISS_TAG: String = "recovery.verify.missed"

/** Test handle for the state where the check itself could not run. */
const val RECOVERY_VERIFY_ERROR_TAG: String = "recovery.verify.error"

/**
 * Proving the recovery secret still opens the account, without asking anybody's permission.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. The "works with no connection" line is on screen at all times rather than in a
 * help article: it is the single fact that distinguishes this check from every password box
 * the user has ever met, and it is the reason a wrong answer here costs nothing.
 *
 * @param state everything to draw.
 * @param onSecretChange edits the passphrase.
 * @param onRevealChange shows or masks it.
 * @param onVerify runs the local check.
 * @param onReset clears the field for another attempt.
 * @param onRewrap opens the guided re-wrap, offered once attempts keep missing.
 */
@Composable
fun RecoveryVerifyScreen(
    state: RecoveryVerifyUiState,
    onSecretChange: (String) -> Unit,
    onRevealChange: (Boolean) -> Unit,
    onVerify: () -> Unit,
    onReset: () -> Unit,
    onRewrap: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(RECOVERY_VERIFY_TAG)) {
        CapsuleTopAppBar(title = stringResource(R.string.android_destination_recovery_verify))
        Column(
            modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
            verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
        ) {
            when (state.phase) {
                VerifyPhase.Checking -> VerifyChecking()
                VerifyPhase.Verified -> VerifyPassed(state, onReset)
                VerifyPhase.Failed -> VerifyUnavailable(state, onReset)
                VerifyPhase.Editing, VerifyPhase.NotVerified ->
                    VerifyForm(state, onSecretChange, onRevealChange, onVerify)
            }
            if (state.phase == VerifyPhase.NotVerified) VerifyMissed(state, onRewrap)
        }
    }
}

@Composable
private fun VerifyChecking() {
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth(),
        title = stringResource(R.string.android_recovery_verify_checking),
        contentDescription = stringResource(R.string.android_recovery_verify_checking),
    )
}

@Composable
private fun VerifyForm(
    state: RecoveryVerifyUiState,
    onSecretChange: (String) -> Unit,
    onRevealChange: (Boolean) -> Unit,
    onVerify: () -> Unit,
) {
    Text(text = stringResource(R.string.android_recovery_verify_body), style = MaterialTheme.typography.bodyMedium)
    Text(
        text = stringResource(R.string.android_recovery_verify_offline_note),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurface,
    )
    OutlinedTextField(
        value = state.secret,
        onValueChange = onSecretChange,
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_VERIFY_FIELD_TAG),
        label = { Text(text = stringResource(R.string.android_recovery_verify_field_label)) },
        supportingText = {
            if (state.secretMissing) Text(text = stringResource(R.string.android_recovery_verify_empty))
        },
        isError = state.secretMissing,
        // A recovery phrase is long, written on paper, and typed once — the case where masking
        // costs more than it protects. It is masked by default all the same, because the same
        // screen is reachable with someone standing behind you.
        visualTransformation =
            if (state.revealed) VisualTransformation.None else PasswordVisualTransformation(),
        colors = capsuleFieldColors(),
    )
    CapsuleToggleButton(
        checked = state.revealed,
        onCheckedChange = onRevealChange,
        label =
            stringResource(
                if (state.revealed) R.string.android_recovery_verify_hide else R.string.android_recovery_verify_reveal,
            ),
    )
    CapsuleButton(
        onClick = onVerify,
        label = stringResource(R.string.android_recovery_verify_submit),
        enabled = state.canSubmit,
    )
}

/**
 * A miss.
 *
 * Warning rather than error, and the wording says twice that nothing was sent and nothing is
 * locked. That is not reassurance for its own sake: a user who believes a wrong answer here
 * might cost them their library will stop answering, and the whole cadence exists to be
 * answered.
 */
@Composable
private fun VerifyMissed(
    state: RecoveryVerifyUiState,
    onRewrap: () -> Unit,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_recovery_verify_failed_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_VERIFY_MISS_TAG),
        body =
            stringResource(R.string.android_recovery_verify_failed_body)
                .withArguments("count" to state.consecutiveFailures, "limit" to state.failureLimit),
        severity = CapsuleBannerSeverity.Warning,
        action =
            if (state.rewrapRecommended) {
                {
                    CapsuleButton(
                        onClick = onRewrap,
                        label = stringResource(R.string.android_recovery_action_rewrap),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                }
            } else {
                null
            },
    )
    Text(
        text = stringResource(R.string.android_recovery_verify_refreshing),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurface,
    )
}

@Composable
private fun VerifyPassed(
    state: RecoveryVerifyUiState,
    onReset: () -> Unit,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_recovery_verify_verified_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_VERIFY_OK_TAG),
        body =
            state.nextPromptAt?.let { next ->
                stringResource(R.string.android_recovery_verify_verified_body)
                    .withArguments("date" to formattedDate(next))
            },
        severity = CapsuleBannerSeverity.Info,
        action = {
            CapsuleButton(
                onClick = onReset,
                label = stringResource(R.string.common_close),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

/**
 * The check could not run at all.
 *
 * Distinct from a miss, and it has to be: "your secret is wrong" and "we could not read the
 * blob to compare against" are opposite conclusions, and telling someone the first when the
 * second happened is how a correct passphrase gets thrown away.
 */
@Composable
private fun VerifyUnavailable(
    state: RecoveryVerifyUiState,
    onReset: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_recovery_verify_error_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_VERIFY_ERROR_TAG),
        body = localizedErrorMessage(state.failure?.code),
        action = {
            CapsuleButton(
                onClick = onReset,
                label = stringResource(R.string.android_recovery_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}
