package com.justin13888.capsule.feature.moderation

import androidx.annotation.StringRes
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
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/** Test handle for the whole appeal surface. */
const val MODERATION_APPEAL_TAG: String = "moderation.appeal"

/** Test handle for the statement field. */
const val MODERATION_APPEAL_STATEMENT_TAG: String = "moderation.appeal.statement"

/** Test handle for the loading state. */
const val MODERATION_APPEAL_LOADING_TAG: String = "moderation.appeal.loading"

/** Test handle for the success state. */
const val MODERATION_APPEAL_SENT_TAG: String = "moderation.appeal.sent"

/** Test handle for the error state, whether transport or proof. */
const val MODERATION_APPEAL_ERROR_TAG: String = "moderation.appeal.error"

/** Test handle for the empty state — a record with nothing on it. */
const val MODERATION_APPEAL_EMPTY_TAG: String = "moderation.appeal.empty"

/**
 * The account's moderation record, and the appeal against one entry of it.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. The record is shown above the form, not behind a link: an appeal is a reply,
 * and a reply written without the thing it replies to in view is a worse reply.
 *
 * @param state everything to draw.
 * @param onStatementChange edits what the administrators will read.
 * @param onSubmit signs a fresh challenge and files it.
 * @param onRetry re-reads the record, or puts the form back after a refusal.
 */
@Composable
fun AppealScreen(
    state: AppealUiState,
    onStatementChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(MODERATION_APPEAL_TAG)) {
        CapsuleTopAppBar(title = stringResource(R.string.android_destination_moderation_appeal))
        when {
            state.isLoading -> AppealLoadingState()
            state.failure != null && state.events.isEmpty() -> AppealRecordFailure(state, onRetry)
            state.isEmpty -> AppealEmptyState()
            else -> AppealBody(state, onStatementChange, onSubmit, onRetry)
        }
    }
}

@Composable
private fun AppealLoadingState() {
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_APPEAL_LOADING_TAG),
        title = stringResource(R.string.android_moderation_appeal_loading),
        contentDescription = stringResource(R.string.android_moderation_appeal_loading),
    )
}

@Composable
private fun AppealEmptyState() {
    CapsuleEmptyState(
        title = stringResource(R.string.android_moderation_notices_empty_title),
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_APPEAL_EMPTY_TAG),
        body = stringResource(R.string.android_moderation_notices_empty_body),
    )
}

@Composable
private fun AppealRecordFailure(
    state: AppealUiState,
    onRetry: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_moderation_notices_error_title),
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_APPEAL_ERROR_TAG),
        body = localizedErrorMessage(state.failure?.code),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_moderation_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun AppealBody(
    state: AppealUiState,
    onStatementChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onRetry: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
        verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
    ) {
        state.effects?.let { effects -> ModerationSuspensionBanner(effects = effects) }
        CapsuleSectionHeader(title = stringResource(R.string.android_moderation_notices_title))
        Text(
            text = stringResource(R.string.android_moderation_notices_body),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        state.events.forEach { event -> ModerationNoticeCard(event = event) }
        AppealForm(state, onStatementChange, onSubmit, onRetry)
    }
}

/**
 * The appeal itself.
 *
 * Present only once the case is on the record. An appeal form against nothing would collect a
 * statement the client has no way to route, and telling the user afterwards is strictly worse
 * than not offering it.
 */
@Composable
private fun AppealForm(
    state: AppealUiState,
    onStatementChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onRetry: () -> Unit,
) {
    if (state.caseMissing) {
        Text(
            text = stringResource(R.string.android_moderation_appeal_case_missing),
            style = MaterialTheme.typography.bodyMedium,
        )
        return
    }
    if (state.case == null) return
    CapsuleSectionHeader(title = stringResource(R.string.android_moderation_appeal_case_label))
    when (state.phase) {
        AppealPhase.Sent -> AppealSentState(state)
        AppealPhase.Proving -> AppealProgress(R.string.android_moderation_appeal_proving)
        AppealPhase.Sending -> AppealProgress(R.string.android_moderation_appeal_sending)
        AppealPhase.Failed -> AppealFailureState(state, onRetry)
        AppealPhase.Editing -> AppealEditor(state, onStatementChange, onSubmit)
    }
}

@Composable
private fun AppealEditor(
    state: AppealUiState,
    onStatementChange: (String) -> Unit,
    onSubmit: () -> Unit,
) {
    Text(
        text = stringResource(R.string.android_moderation_appeal_body),
        style = MaterialTheme.typography.bodyMedium,
    )
    OutlinedTextField(
        value = state.statement,
        onValueChange = onStatementChange,
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_APPEAL_STATEMENT_TAG),
        label = { Text(text = stringResource(R.string.android_moderation_appeal_statement_label)) },
        supportingText = {
            if (state.statementMissing) {
                Text(text = stringResource(R.string.android_moderation_appeal_statement_required))
            }
        },
        isError = state.statementMissing,
        colors = capsuleFieldColors(),
    )
    CapsuleButton(
        onClick = onSubmit,
        label = stringResource(R.string.android_moderation_appeal_prove),
        enabled = state.canSubmit,
    )
}

@Composable
private fun AppealProgress(
    @StringRes textRes: Int,
) {
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth(),
        title = stringResource(textRes),
        contentDescription = stringResource(textRes),
    )
}

@Composable
private fun AppealSentState(state: AppealUiState) {
    CapsuleEmptyState(
        title = stringResource(R.string.android_moderation_appeal_sent_title),
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_APPEAL_SENT_TAG),
        body =
            stringResource(R.string.android_moderation_appeal_sent_body)
                .withArguments("reference" to (state.reference ?: state.caseId)),
    )
}

/**
 * A refused appeal, split by whether the master key was what failed.
 *
 * A rejected proof says plainly that nothing was sent. Every other failure leaves that
 * question open, so its copy is the generic one — telling someone to retry a submission that
 * may already have arrived is how an admin queue fills with duplicates.
 */
@Composable
private fun AppealFailureState(
    state: AppealUiState,
    onRetry: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_moderation_appeal_error_title),
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_APPEAL_ERROR_TAG),
        body =
            if (state.proofRejected) {
                stringResource(R.string.android_moderation_appeal_proof_failed)
            } else {
                localizedErrorMessage(state.failure?.code)
            },
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_moderation_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}
