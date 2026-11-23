package com.justin13888.capsule.feature.recovery

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/** Test handle for the whole guardian-share surface. */
const val RECOVERY_SHAMIR_TAG: String = "recovery.shamir"

/** Test handle for the empty state. */
const val RECOVERY_SHAMIR_EMPTY_TAG: String = "recovery.shamir.empty"

/** Test handle for the distribution-progress line. */
const val RECOVERY_SHAMIR_PROGRESS_TAG: String = "recovery.shamir.progress"

/** Test handle for the error state. */
const val RECOVERY_SHAMIR_ERROR_TAG: String = "recovery.shamir.error"

/** Test handle for one share card: `recovery.share:<index>`. */
fun recoveryShareTag(index: Int): String = "recovery.share:$index"

/** Everything the guardian-share screen can be asked to do, in one object. */
@Immutable
class ShamirActions(
    /** Changes how many shares reconstruct the secret. */
    val onThresholdChange: (Int) -> Unit,
    /** Changes how many exist. */
    val onTotalChange: (Int) -> Unit,
    /** Splits a secret into the proposed scheme. */
    val onGenerate: () -> Unit,
    /** Opens or closes the holder field for one share. */
    val onEditHolder: (Int?) -> Unit,
    /** Edits who is taking that share. */
    val onHolderDraftChange: (String) -> Unit,
    /** Records that it has left this device. */
    val onMarkDelivered: (Int) -> Unit,
)

/**
 * Splitting the recovery secret between people or places the user trusts.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. Distribution is tracked as first-class state rather than left to the user's
 * memory, because shares that never leave the drawer beside the phone are not a second
 * recovery path — they are the same single point of failure, spread over three envelopes.
 *
 * @param state everything to draw.
 * @param actions what the user can do.
 */
@Composable
fun ShamirScreen(
    state: ShamirUiState,
    actions: ShamirActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(RECOVERY_SHAMIR_TAG)) {
        CapsuleTopAppBar(title = stringResource(R.string.android_destination_shamir))
        Column(
            modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
            verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
        ) {
            Text(
                text =
                    stringResource(R.string.android_recovery_shamir_body)
                        .withArguments("threshold" to state.threshold, "total" to state.total),
                style = MaterialTheme.typography.bodyMedium,
            )
            SchemeControls(state, actions)
            state.failure?.let { failure -> ShamirFailure(failure) }
            when {
                state.isEmpty -> ShamirEmptyState()
                else -> ShareList(state, actions)
            }
        }
    }
}

/**
 * The `m`-of-`n` controls.
 *
 * Steppers rather than free entry, and clamped by the view model rather than validated after
 * the fact, so an impossible scheme — needing four of three — is unreachable rather than
 * refused. The validation line is still present for the one case a stepper cannot prevent: a
 * threshold of one, which is not a split at all.
 */
@Composable
private fun SchemeControls(
    state: ShamirUiState,
    actions: ShamirActions,
) {
    Stepper(
        labelRes = R.string.android_recovery_shamir_threshold_label,
        value = state.threshold,
        decreaseRes = R.string.android_recovery_shamir_threshold_fewer,
        increaseRes = R.string.android_recovery_shamir_threshold_more,
        onChange = actions.onThresholdChange,
    )
    Stepper(
        labelRes = R.string.android_recovery_shamir_total_label,
        value = state.total,
        decreaseRes = R.string.android_recovery_shamir_total_fewer,
        increaseRes = R.string.android_recovery_shamir_total_more,
        onChange = actions.onTotalChange,
    )
    if (!state.isSchemeValid) {
        CapsuleBanner(
            title = stringResource(R.string.android_recovery_shamir_invalid),
            modifier = Modifier.fillMaxWidth(),
            severity = CapsuleBannerSeverity.Warning,
        )
    }
    CapsuleButton(
        onClick = actions.onGenerate,
        label =
            stringResource(
                if (state.isEmpty) {
                    R.string.android_recovery_shamir_generate
                } else {
                    R.string.android_recovery_shamir_regenerate
                },
            ),
        enabled = state.isSchemeValid,
    )
}

@Composable
private fun Stepper(
    @StringRes labelRes: Int,
    value: Int,
    @StringRes decreaseRes: Int,
    @StringRes increaseRes: Int,
    onChange: (Int) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(ScreenSpacing)) {
        Text(
            text = stringResource(labelRes) + SEPARATOR + value,
            style = MaterialTheme.typography.bodyMedium,
        )
        CapsuleButton(
            onClick = { onChange(value - 1) },
            label = stringResource(decreaseRes),
            emphasis = CapsuleButtonEmphasis.Secondary,
        )
        CapsuleButton(
            onClick = { onChange(value + 1) },
            label = stringResource(increaseRes),
            emphasis = CapsuleButtonEmphasis.Secondary,
        )
    }
}

@Composable
private fun ShamirEmptyState() {
    CapsuleEmptyState(
        title = stringResource(R.string.android_recovery_shamir_empty_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_SHAMIR_EMPTY_TAG),
        body = stringResource(R.string.android_recovery_shamir_empty_body),
    )
}

@Composable
private fun ShamirFailure(failure: RecoveryFailure) {
    CapsuleErrorState(
        title = stringResource(R.string.android_recovery_shamir_error_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_SHAMIR_ERROR_TAG),
        body = localizedErrorMessage(failure.code),
    )
}

@Composable
private fun ShareList(
    state: ShamirUiState,
    actions: ShamirActions,
) {
    val enrollment = state.enrollment ?: return
    CapsuleSectionHeader(title = stringResource(R.string.android_recovery_action_shamir))
    DistributionProgress(state, enrollment)
    enrollment.shares.forEach { share ->
        ShareCard(share = share, total = enrollment.total, state = state, actions = actions)
    }
}

/**
 * How far the hand-out has got.
 *
 * Stated against the threshold rather than the total, because that is the number at which
 * this stops being paperwork and starts being a recovery path — and a user who stops after
 * two of three has done the thing that matters.
 */
@Composable
private fun DistributionProgress(
    state: ShamirUiState,
    enrollment: ShamirEnrollment,
) {
    Text(
        text =
            stringResource(R.string.android_recovery_shamir_progress)
                .withArguments("delivered" to enrollment.deliveredCount, "total" to enrollment.total),
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(RECOVERY_SHAMIR_PROGRESS_TAG),
    )
    Text(
        text =
            if (state.isDistributed) {
                stringResource(R.string.android_recovery_shamir_complete)
            } else {
                stringResource(R.string.android_recovery_shamir_incomplete)
                    .withArguments("threshold" to enrollment.threshold)
            },
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurface,
    )
}

@Composable
private fun ShareCard(
    share: ShamirShare,
    total: Int,
    state: ShamirUiState,
    actions: ShamirActions,
) {
    val title =
        stringResource(R.string.android_recovery_shamir_share_title)
            .withArguments("index" to share.index, "total" to total)
    CapsuleCard(
        modifier = Modifier.fillMaxWidth().testTag(recoveryShareTag(share.index)),
        contentDescription = title,
    ) {
        Text(text = title, style = MaterialTheme.typography.titleSmall)
        Text(
            text = share.payload,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
        )
        if (share.isDelivered) {
            Text(
                text =
                    stringResource(R.string.android_recovery_shamir_delivered)
                        .withArguments("holder" to (share.holder ?: "")),
                style = MaterialTheme.typography.bodyMedium,
            )
        } else if (state.editingShare == share.index) {
            OutlinedTextField(
                value = state.holderDraft,
                onValueChange = actions.onHolderDraftChange,
                modifier = Modifier.fillMaxWidth(),
                label = { Text(text = stringResource(R.string.android_recovery_shamir_share_holder)) },
                singleLine = true,
                colors = capsuleFieldColors(),
            )
            CapsuleButton(
                onClick = { actions.onMarkDelivered(share.index) },
                label = stringResource(R.string.android_recovery_shamir_mark_delivered),
                enabled = state.holderDraft.isNotBlank(),
                contentDescription =
                    stringResource(R.string.android_recovery_shamir_delivered_description)
                        .withArguments("index" to share.index),
            )
        } else {
            Text(
                text = stringResource(R.string.android_recovery_shamir_pending),
                style = MaterialTheme.typography.bodyMedium,
            )
            // The spoken description names the share. A column of buttons that all announce
            // "Who holds this share" is a column a screen-reader user cannot act on.
            CapsuleButton(
                onClick = { actions.onEditHolder(share.index) },
                label = stringResource(R.string.android_recovery_shamir_share_holder),
                emphasis = CapsuleButtonEmphasis.Secondary,
                contentDescription =
                    stringResource(R.string.android_recovery_shamir_record_holder_description)
                        .withArguments("index" to share.index),
            )
        }
    }
}
