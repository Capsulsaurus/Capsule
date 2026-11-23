package com.justin13888.capsule.feature.moderation

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
import com.justin13888.capsule.core.designsystem.component.CapsuleCheckbox
import com.justin13888.capsule.core.designsystem.component.CapsuleChip
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/** Test handle for the whole report surface. */
const val MODERATION_REPORT_TAG: String = "moderation.report"

/** Test handle for the field carrying the one thing that leaves this device. */
const val MODERATION_REPORT_HASH_TAG: String = "moderation.report.hash"

/** Test handle for the reason field. */
const val MODERATION_REPORT_REASON_TAG: String = "moderation.report.reason"

/** Test handle for the success state. */
const val MODERATION_REPORT_SENT_TAG: String = "moderation.report.sent"

/** Test handle for the refusal state. */
const val MODERATION_REPORT_ERROR_TAG: String = "moderation.report.error"

/**
 * Everything the report screen can ask for, in one object.
 *
 * Grouped so the screen stays a plain function of state plus intent, and so adding a field
 * later is one parameter here rather than one more argument at every call site.
 */
@Immutable
class ReportActions(
    /** Switches between reporting an asset and reporting the account behind it. */
    val onKindChange: (ReportSubjectKind) -> Unit,
    /** Edits the ciphertext hash. */
    val onContentHashChange: (String) -> Unit,
    /** Edits the album pointer. */
    val onPointerChange: (String) -> Unit,
    /** Edits the reason. */
    val onReasonChange: (String) -> Unit,
    /** Chooses whether a personal block accompanies the report. */
    val onAlsoBlockChange: (Boolean) -> Unit,
    /** Sends it. */
    val onSubmit: () -> Unit,
    /** Puts the form back after a refusal. */
    val onRetry: () -> Unit,
)

/**
 * Reporting one asset, or the account behind it, to the server that hosts it.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. The screen shows the exact payload — a hash and a pointer — rather than
 * describing it, because "we never send your photo" is a claim, and a visible hash is
 * evidence.
 *
 * @param state everything to draw.
 * @param actions what the user can do to the form itself.
 * @param onDone leaves, once there is nothing left to do here. Separate from [actions]
 *   because it is the caller's navigation rather than this screen's behaviour.
 */
@Composable
fun ReportScreen(
    state: ReportUiState,
    actions: ReportActions,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(MODERATION_REPORT_TAG)) {
        CapsuleTopAppBar(title = stringResource(R.string.android_destination_moderation_report))
        when {
            state.hasNoSubject -> ReportNoSubjectState()
            state.phase == ReportPhase.Sending -> ReportSendingState()
            state.phase == ReportPhase.Sent -> ReportSentState(onDone)
            state.phase == ReportPhase.Failed -> ReportFailureState(state.failure, actions.onRetry)
            else -> ReportForm(state, actions)
        }
    }
}

@Composable
private fun ReportNoSubjectState() {
    CapsuleEmptyState(
        title = stringResource(R.string.android_moderation_report_subject_missing),
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun ReportSendingState() {
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth(),
        title = stringResource(R.string.android_moderation_report_sending),
        contentDescription = stringResource(R.string.android_moderation_report_sending),
    )
}

@Composable
private fun ReportSentState(onDone: () -> Unit) {
    CapsuleEmptyState(
        title = stringResource(R.string.android_moderation_report_sent_title),
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_REPORT_SENT_TAG),
        body = stringResource(R.string.android_moderation_report_sent_body),
        action = {
            CapsuleButton(
                onClick = onDone,
                label = stringResource(R.string.common_close),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun ReportFailureState(
    failure: ModerationFailure?,
    onRetry: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_moderation_report_error_title),
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_REPORT_ERROR_TAG),
        // The localized `error.*` code, never a status: the code is the contract, and a status
        // is a transport detail the user can do nothing with.
        body = localizedErrorMessage(failure?.code),
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
private fun ReportForm(
    state: ReportUiState,
    actions: ReportActions,
) {
    Column(
        modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
        verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_moderation_report_body),
            style = MaterialTheme.typography.bodyMedium,
        )
        ReportKindRow(state.kind, actions.onKindChange)
        ReportFields(state, actions)
        if (state.hashMissing) {
            CapsuleBanner(
                title = stringResource(R.string.android_moderation_report_hash_required),
                modifier = Modifier.fillMaxWidth(),
                severity = CapsuleBannerSeverity.Warning,
            )
        }
        ReportBlockOption(state, actions.onAlsoBlockChange)
        Text(
            text = stringResource(R.string.android_moderation_report_privacy_note),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
        )
        CapsuleButton(
            onClick = actions.onSubmit,
            label = stringResource(R.string.android_moderation_report_submit),
            enabled = state.canSubmit,
        )
    }
}

/**
 * The two things a report can be about.
 *
 * Chips rather than a dropdown: there are exactly two, they are mutually exclusive, and the
 * choice changes which field below is required — all three of which argue for showing both
 * options at once rather than hiding one behind a tap.
 */
@Composable
private fun ReportKindRow(
    kind: ReportSubjectKind,
    onKindChange: (ReportSubjectKind) -> Unit,
) {
    Text(
        text = stringResource(R.string.android_moderation_report_kind_label),
        style = MaterialTheme.typography.labelLarge,
    )
    Row(horizontalArrangement = Arrangement.spacedBy(ScreenSpacing)) {
        CapsuleChip(
            label = stringResource(R.string.android_moderation_report_kind_content),
            selected = kind == ReportSubjectKind.Content,
            onSelectedChange = { onKindChange(ReportSubjectKind.Content) },
        )
        CapsuleChip(
            label = stringResource(R.string.android_moderation_report_kind_user),
            selected = kind == ReportSubjectKind.Account,
            onSelectedChange = { onKindChange(ReportSubjectKind.Account) },
        )
    }
}

/**
 * A field's hint, at full text contrast.
 *
 * Material's own `supportingText` slot renders in `onSurfaceVariant`, which lands just under
 * the 4.5:1 the accessibility check demands of small text. These hints carry the promise that
 * a pointer grants no access, so they are exactly the lines that must survive a low-contrast
 * screen in daylight.
 */
@Composable
private fun FieldHint(
    @StringRes textRes: Int,
) {
    Text(text = stringResource(textRes), color = MaterialTheme.colorScheme.onSurface)
}

@Composable
private fun ReportFields(
    state: ReportUiState,
    actions: ReportActions,
) {
    if (state.kind == ReportSubjectKind.Account && state.accountId.isNotBlank()) {
        OutlinedTextField(
            value = state.accountId,
            onValueChange = {},
            modifier = Modifier.fillMaxWidth(),
            enabled = false,
            label = { Text(text = stringResource(R.string.android_moderation_report_subject_user_label)) },
            singleLine = true,
            colors = capsuleFieldColors(),
        )
    }
    OutlinedTextField(
        value = state.contentHash,
        onValueChange = actions.onContentHashChange,
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_REPORT_HASH_TAG),
        label = { Text(text = stringResource(R.string.android_moderation_report_subject_content_label)) },
        isError = state.hashMissing,
        singleLine = true,
        colors = capsuleFieldColors(),
    )
    OutlinedTextField(
        value = state.albumPointer,
        onValueChange = actions.onPointerChange,
        modifier = Modifier.fillMaxWidth(),
        label = { Text(text = stringResource(R.string.android_moderation_report_pointer_label)) },
        supportingText = { FieldHint(R.string.android_moderation_report_pointer_hint) },
        singleLine = true,
        colors = capsuleFieldColors(),
    )
    OutlinedTextField(
        value = state.reason,
        onValueChange = actions.onReasonChange,
        modifier = Modifier.fillMaxWidth().testTag(MODERATION_REPORT_REASON_TAG),
        label = { Text(text = stringResource(R.string.android_moderation_report_reason_label)) },
        supportingText = { FieldHint(R.string.android_moderation_report_reason_hint) },
        colors = capsuleFieldColors(),
    )
}

/**
 * The block that rides alongside a report.
 *
 * Offered only when an account is actually known, because a checkbox that would silently do
 * nothing is worse than an absent one. The note beside it says the block is the user's own
 * regardless of what the administrators decide — the two are independent, and conflating them
 * would leave someone waiting on a queue for protection they already have.
 */
@Composable
private fun ReportBlockOption(
    state: ReportUiState,
    onAlsoBlockChange: (Boolean) -> Unit,
) {
    if (!state.canBlock) return
    Row(verticalAlignment = Alignment.CenterVertically) {
        CapsuleCheckbox(
            checked = state.alsoBlock,
            onCheckedChange = onAlsoBlockChange,
            contentDescription = stringResource(R.string.android_moderation_report_also_block),
        )
        Text(
            text = stringResource(R.string.android_moderation_report_also_block),
            style = MaterialTheme.typography.bodyMedium,
        )
    }
    Text(
        text = stringResource(R.string.android_moderation_report_blocked_note),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurface,
    )
}
