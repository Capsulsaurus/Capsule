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
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.RestoreReport

/** Test handle for the whole restore surface. */
const val RECOVERY_RESTORE_TAG: String = "recovery.restore"

/** Test handle for the archive-path field. */
const val RECOVERY_RESTORE_PATH_TAG: String = "recovery.restore.path"

/** Test handle for the preview card. */
const val RECOVERY_RESTORE_PREVIEW_TAG: String = "recovery.restore.preview"

/** Test handle for the dry-run report. */
const val RECOVERY_RESTORE_REPORT_TAG: String = "recovery.restore.report"

/** Test handle for the typed-confirmation field. */
const val RECOVERY_RESTORE_CONFIRM_TAG: String = "recovery.restore.confirm"

/** Test handle for the finished state. */
const val RECOVERY_RESTORE_DONE_TAG: String = "recovery.restore.done"

/** Test handle for the error state. */
const val RECOVERY_RESTORE_ERROR_TAG: String = "recovery.restore.error"

/** Test handle for the banner refusing an archive that did not authenticate. */
const val RECOVERY_RESTORE_UNAUTH_TAG: String = "recovery.restore.unauthenticated"

/** Test handle for the standing rule that newer local state wins. */
const val RECOVERY_RESTORE_RULE_TAG: String = "recovery.restore.rule"

/** Everything the restore screen can be asked to do, in one object. */
@Immutable
class BackupRestoreActions(
    /** Edits where the archive is. */
    val onPathChange: (String) -> Unit,
    /** Reads and authenticates the manifest. */
    val onPreview: () -> Unit,
    /** Verifies everything and computes the diff. */
    val onDryRun: () -> Unit,
    /** Moves to the typed confirmation. */
    val onBeginCommit: () -> Unit,
    /** Edits the typed confirmation. */
    val onConfirmationChange: (String) -> Unit,
    /** Applies the report the user has read, given the localized phrase they had to type. */
    val onCommit: (String) -> Unit,
    /** Opens or closes the conflict-merge surface. */
    val onReviewConflicts: (Boolean) -> Unit,
)

/**
 * Reading an encrypted archive back, one deliberate rung at a time.
 *
 * Stateless, so every rung is reachable from a preview and a test without a view model. The
 * reconciliation rule is on screen from the dry run onward rather than buried in a
 * confirmation dialog: "anything newer here stays" is the fact that decides whether a user
 * should be frightened of this screen, and they should not be.
 *
 * @param state everything to draw.
 * @param actions what the user can do.
 * @param onDecision records what the user decided about one conflicting asset.
 * @param onApplyMerge turns those decisions into a plan.
 * @param onReset drops everything and starts from an empty path. Separate from [actions]
 *   because it is this screen abandoning its own work rather than a rung of the ladder.
 */
@Composable
fun BackupRestoreScreen(
    state: BackupRestoreUiState,
    actions: BackupRestoreActions,
    onDecision: (String, RestoreDecision) -> Unit,
    onApplyMerge: () -> Unit,
    onReset: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (state.reviewingConflicts) {
        RestoreConflictScreen(
            state = state,
            onDecision = onDecision,
            onApply = onApplyMerge,
            onBack = { actions.onReviewConflicts(false) },
            modifier = modifier,
        )
        return
    }
    Column(modifier = modifier.fillMaxSize().testTag(RECOVERY_RESTORE_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_backup_restore),
            subtitle = stringResource(state.stage.labelRes()),
        )
        Column(
            modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
            verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
        ) {
            Text(
                text = stringResource(R.string.android_recovery_restore_body),
                style = MaterialTheme.typography.bodyMedium,
            )
            RestorePathField(state, actions)
            state.busy?.let { busy -> RestoreBusyState(busy) }
            state.failure?.let { failure -> RestoreFailure(failure, onReset) }
            RestorePreview(state, actions)
            RestoreReportCard(state, actions)
            RestoreConfirmation(state, actions)
            RestoreOutcome(state)
        }
    }
}

/** The catalogue row naming one rung. */
private fun RestoreStage.labelRes(): Int =
    when (this) {
        RestoreStage.Choosing, RestoreStage.Previewed -> R.string.android_recovery_restore_stage_preview
        RestoreStage.DryRun -> R.string.android_recovery_restore_stage_dry_run
        RestoreStage.Confirming, RestoreStage.Committed -> R.string.android_recovery_restore_stage_commit
    }

@Composable
private fun RestorePathField(
    state: BackupRestoreUiState,
    actions: BackupRestoreActions,
) {
    OutlinedTextField(
        value = state.path,
        onValueChange = actions.onPathChange,
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_RESTORE_PATH_TAG),
        label = { Text(text = stringResource(R.string.android_recovery_restore_path_label)) },
        supportingText = {
            if (state.pathMissing) Text(text = stringResource(R.string.android_recovery_restore_path_required))
        },
        isError = state.pathMissing,
        singleLine = true,
        colors = capsuleFieldColors(),
    )
    if (state.stage == RestoreStage.Choosing) {
        CapsuleButton(
            onClick = actions.onPreview,
            label = stringResource(R.string.android_recovery_restore_preview_run),
            enabled = state.canPreview,
        )
    }
}

@Composable
private fun RestoreBusyState(busy: RestoreBusy) {
    val textRes =
        when (busy) {
            RestoreBusy.Previewing -> R.string.android_recovery_restore_preview_run
            RestoreBusy.DryRunning -> R.string.android_recovery_restore_dry_run_working
            RestoreBusy.Committing -> R.string.android_recovery_restore_committing
        }
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth(),
        title = stringResource(textRes),
        contentDescription = stringResource(textRes),
    )
}

@Composable
private fun RestoreFailure(
    failure: RecoveryFailure,
    onReset: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_recovery_restore_error_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_RESTORE_ERROR_TAG),
        body = localizedErrorMessage(failure.code),
        action = {
            CapsuleButton(
                onClick = onReset,
                label = stringResource(R.string.android_recovery_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

/**
 * What the manifest says, and whether it can be believed.
 *
 * An archive that failed either authentication stops here. The wording says the restore is
 * refused rather than offering to proceed anyway, because "proceed anyway" over a manifest
 * whose signature did not verify is proceeding on the word of whoever tampered with it.
 */
@Composable
private fun RestorePreview(
    state: BackupRestoreUiState,
    actions: BackupRestoreActions,
) {
    val artifact = state.artifact ?: return
    CapsuleCard(modifier = Modifier.fillMaxWidth().testTag(RECOVERY_RESTORE_PREVIEW_TAG)) {
        Text(
            text = stringResource(R.string.android_recovery_restore_preview_title),
            style = MaterialTheme.typography.titleSmall,
        )
        Text(text = artifactSummary(artifact), style = MaterialTheme.typography.bodyMedium)
        Text(
            text =
                stringResource(R.string.android_recovery_restore_preview_exported_by)
                    .withArguments(
                        "date" to formattedDate(artifact.exportedAt),
                        "device" to artifact.exporterDevice.value,
                    ),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
        )
        Text(
            text = stringResource(R.string.android_recovery_restore_preview_note),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
        )
    }
    if (state.isAuthenticated) {
        Text(
            text = stringResource(R.string.android_recovery_artifact_authenticated),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
        )
    } else {
        CapsuleBanner(
            title = stringResource(R.string.android_recovery_artifact_unauthenticated),
            modifier = Modifier.fillMaxWidth().testTag(RECOVERY_RESTORE_UNAUTH_TAG),
            severity = CapsuleBannerSeverity.Error,
        )
        return
    }
    if (state.stage == RestoreStage.Previewed) {
        CapsuleButton(
            onClick = actions.onDryRun,
            label = stringResource(R.string.android_recovery_restore_dry_run_run),
            enabled = state.canDryRun,
        )
    }
}

/** The four numbers, plus the rule that decides what they mean. */
@Composable
private fun RestoreReportCard(
    state: BackupRestoreUiState,
    actions: BackupRestoreActions,
) {
    val report = state.report ?: return
    if (state.stage == RestoreStage.Committed) return
    CapsuleCard(modifier = Modifier.fillMaxWidth().testTag(RECOVERY_RESTORE_REPORT_TAG)) {
        Text(
            text = stringResource(R.string.android_recovery_restore_dry_run_title),
            style = MaterialTheme.typography.titleSmall,
        )
        ReportLines(report)
        Text(
            text = stringResource(R.string.android_recovery_restore_no_write),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
        )
    }
    Text(
        text = stringResource(R.string.android_recovery_restore_newer_local_wins),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.testTag(RECOVERY_RESTORE_RULE_TAG),
    )
    state.merge?.let { plan -> MergeOutcome(plan) }
    if (report.conflicts.isNotEmpty()) {
        CapsuleButton(
            onClick = { actions.onReviewConflicts(true) },
            label =
                stringResource(R.string.android_recovery_restore_review_conflicts)
                    .withArguments("count" to report.conflicts.size),
            emphasis = CapsuleButtonEmphasis.Secondary,
        )
    }
    if (state.stage == RestoreStage.DryRun) {
        CapsuleButton(
            onClick = actions.onBeginCommit,
            label = stringResource(R.string.android_recovery_restore_stage_commit),
            enabled = state.canConfirm,
        )
    }
}

@Composable
private fun ReportLines(report: RestoreReport) {
    Text(
        text = stringResource(R.string.android_recovery_restore_verified).withArguments("count" to report.verified),
        style = MaterialTheme.typography.bodyMedium,
    )
    Text(
        text =
            stringResource(R.string.android_recovery_restore_would_add)
                .withArguments("count" to report.wouldAdd.size),
        style = MaterialTheme.typography.bodyMedium,
    )
    Text(
        text =
            stringResource(R.string.android_recovery_restore_identical)
                .withArguments("count" to report.identical.size),
        style = MaterialTheme.typography.bodyMedium,
    )
    Text(
        text =
            stringResource(R.string.android_recovery_restore_conflicts)
                .withArguments("count" to report.conflicts.size),
        style = MaterialTheme.typography.bodyMedium,
    )
}

@Composable
private fun MergeOutcome(plan: RestoreMergePlan) {
    Text(
        text =
            stringResource(R.string.android_recovery_restore_merge_summary)
                .withArguments("kept" to plan.keptLocal.size, "rolled" to plan.rolledBack.size),
        style = MaterialTheme.typography.bodyMedium,
    )
}

/**
 * The typed confirmation.
 *
 * The phrase is a catalogue string, so it is whatever the reader's language calls it, and the
 * comparison happens in the view model against the same localized value. A confirmation
 * phrase that only worked in English would be a confirmation most of the world types by
 * copying shapes they cannot read.
 */
@Composable
private fun RestoreConfirmation(
    state: BackupRestoreUiState,
    actions: BackupRestoreActions,
) {
    if (state.stage != RestoreStage.Confirming) return
    val phrase = stringResource(R.string.android_recovery_restore_confirm_phrase)
    Text(
        text = stringResource(R.string.android_recovery_restore_commit_title),
        style = MaterialTheme.typography.titleSmall,
    )
    Text(
        text =
            stringResource(R.string.android_recovery_restore_commit_body)
                .withArguments("count" to state.wouldAddCount, "phrase" to phrase),
        style = MaterialTheme.typography.bodyMedium,
    )
    OutlinedTextField(
        value = state.confirmation,
        onValueChange = actions.onConfirmationChange,
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_RESTORE_CONFIRM_TAG),
        label = { Text(text = stringResource(R.string.android_recovery_restore_confirm_label)) },
        supportingText = {
            if (state.confirmationWrong) {
                Text(
                    text =
                        stringResource(R.string.android_recovery_restore_confirm_mismatch)
                            .withArguments("phrase" to phrase),
                )
            }
        },
        isError = state.confirmationWrong,
        singleLine = true,
        colors = capsuleFieldColors(),
    )
    CapsuleButton(
        onClick = { actions.onCommit(phrase) },
        label = stringResource(R.string.android_recovery_restore_commit_run),
        enabled = state.canConfirm,
    )
}

@Composable
private fun RestoreOutcome(state: BackupRestoreUiState) {
    val committed = state.committed ?: return
    CapsuleBanner(
        title = stringResource(R.string.android_recovery_restore_done_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_RESTORE_DONE_TAG),
        body =
            if (committed.applied.isEmpty()) {
                stringResource(R.string.android_recovery_restore_nothing_applied)
            } else {
                stringResource(R.string.android_recovery_restore_applied)
                    .withArguments("count" to committed.applied.size)
            },
        severity = CapsuleBannerSeverity.Info,
    )
    Text(
        text = stringResource(R.string.android_recovery_restore_newer_local_wins),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurface,
    )
}
