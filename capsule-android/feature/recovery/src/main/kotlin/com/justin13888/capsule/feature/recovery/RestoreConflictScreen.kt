package com.justin13888.capsule.feature.recovery

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
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleToggleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.RestoreConflict
import com.justin13888.capsule.core.model.RestoreConflictKind
import com.justin13888.capsule.core.model.knownOrNull

/** Test handle for the conflict-merge surface. */
const val RECOVERY_CONFLICTS_TAG: String = "recovery.conflicts"

/** Test handle for its empty state. */
const val RECOVERY_CONFLICTS_EMPTY_TAG: String = "recovery.conflicts.empty"

/** Test handle for the note about what leaving one undecided does. */
const val RECOVERY_CONFLICTS_UNDECIDED_TAG: String = "recovery.conflicts.undecided"

/** Test handle for one conflict card: `recovery.conflict:<assetId>`. */
fun recoveryConflictTag(assetId: String): String = "recovery.conflict:$assetId"

/**
 * The assets where the archive and this device disagree, one decision at a time.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. Nothing on this screen has a default that changes anything: an untouched
 * conflict keeps the copy on the device, so a user who opens this, reads it, and leaves has
 * lost nothing — which is the behaviour that makes it safe to open at all.
 *
 * The divergent case says out loud that taking the older copy may bring back something the
 * user deleted. That is the sentence standing between a six-month-old archive and a
 * resurrected photo somebody deliberately destroyed.
 *
 * @param state everything to draw.
 * @param onDecision records what the user decided about one asset.
 * @param onApply turns the decisions into a plan.
 * @param onBack returns to the report.
 */
@Composable
fun RestoreConflictScreen(
    state: BackupRestoreUiState,
    onDecision: (String, RestoreDecision) -> Unit,
    onApply: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(RECOVERY_CONFLICTS_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_recovery_restore_conflicts_title),
            subtitle = stringResource(R.string.android_destination_backup_restore),
        )
        if (state.conflicts.isEmpty()) {
            CapsuleEmptyState(
                title = stringResource(R.string.android_recovery_restore_merge_empty_title),
                modifier = Modifier.fillMaxWidth().testTag(RECOVERY_CONFLICTS_EMPTY_TAG),
                body = stringResource(R.string.android_recovery_restore_merge_empty_body),
                action = {
                    CapsuleButton(
                        onClick = onBack,
                        label = stringResource(R.string.common_back),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                },
            )
            return
        }
        Column(
            modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
            verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
        ) {
            Text(
                text = stringResource(R.string.android_recovery_restore_conflicts_body),
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                text = stringResource(R.string.android_recovery_restore_newer_local_wins),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurface,
            )
            state.conflicts.forEach { conflict ->
                ConflictCard(
                    conflict = conflict,
                    decision = state.decisions[conflict.assetId.value] ?: RestoreDecision.Undecided,
                    onDecision = onDecision,
                )
            }
            if (state.undecidedCount > 0) {
                Text(
                    text =
                        stringResource(R.string.android_recovery_restore_merge_undecided)
                            .withArguments("count" to state.undecidedCount),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier.testTag(RECOVERY_CONFLICTS_UNDECIDED_TAG),
                )
            }
            CapsuleButton(
                onClick = onApply,
                label = stringResource(R.string.android_recovery_restore_merge_apply),
            )
            CapsuleButton(
                onClick = onBack,
                label = stringResource(R.string.common_back),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        }
    }
}

/**
 * One disagreement, and the two things that can be done about it.
 *
 * Both choices are shown as toggles rather than one being a checkbox, so neither reads as the
 * "normal" one being opted out of. The keep-local toggle is checked while the decision is
 * still undecided, because that is what would happen — showing nothing checked would imply a
 * pending choice that must be made, and no choice must be made here.
 */
@Composable
private fun ConflictCard(
    conflict: RestoreConflict,
    decision: RestoreDecision,
    onDecision: (String, RestoreDecision) -> Unit,
) {
    val id = conflict.assetId.value
    val heading = stringResource(R.string.android_recovery_restore_conflict_asset).withArguments("id" to id)
    CapsuleCard(
        modifier = Modifier.fillMaxWidth().testTag(recoveryConflictTag(id)),
        contentDescription = heading,
    ) {
        Text(text = heading, style = MaterialTheme.typography.titleSmall)
        Text(text = stringResource(conflict.explanationRes()), style = MaterialTheme.typography.bodyMedium)
        // Both spoken descriptions name the asset. Two rows of identically-announced toggles
        // would leave a screen-reader user choosing between four indistinguishable controls,
        // on a screen where the wrong one rolls a photo back.
        CapsuleToggleButton(
            checked = decision != RestoreDecision.UseBackup,
            onCheckedChange = { onDecision(id, RestoreDecision.KeepLocal) },
            label = stringResource(R.string.android_recovery_restore_keep_local),
            contentDescription =
                stringResource(R.string.android_recovery_restore_keep_local_description)
                    .withArguments("id" to id),
        )
        CapsuleToggleButton(
            checked = decision == RestoreDecision.UseBackup,
            onCheckedChange = { onDecision(id, RestoreDecision.UseBackup) },
            label = stringResource(R.string.android_recovery_restore_use_backup),
            contentDescription =
                stringResource(R.string.android_recovery_restore_use_backup_description)
                    .withArguments("id" to id),
        )
    }
}

/**
 * What kind of disagreement this is.
 *
 * A kind this build does not know is named as such rather than hidden: the set is closed and
 * versioned, so an unknown token means a newer client wrote the chain — and quietly dropping
 * the row would present a conflict as though it had been resolved.
 */
private fun RestoreConflict.explanationRes(): Int =
    when (kind.knownOrNull) {
        RestoreConflictKind.LiveHeadNewer -> R.string.android_recovery_restore_conflict_live_newer
        RestoreConflictKind.Divergent -> R.string.android_recovery_restore_conflict_divergent
        RestoreConflictKind.Identical, RestoreConflictKind.AbsentLocally, null ->
            R.string.android_recovery_restore_conflict_unknown
    }
