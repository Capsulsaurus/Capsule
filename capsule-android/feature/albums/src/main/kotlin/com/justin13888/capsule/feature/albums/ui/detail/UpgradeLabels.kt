package com.justin13888.capsule.feature.albums.ui.detail

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.albums.model.UpgradeAbortReason
import com.justin13888.capsule.feature.albums.model.UpgradePhase
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/** What phase the ceremony is in, or `null` while the album is running normally. */
@Composable
internal fun upgradePhaseLabel(phase: UpgradePhase): String? =
    when (phase) {
        UpgradePhase.Idle -> null
        is UpgradePhase.Quiescing -> stringResource(R.string.albums_upgrade_phase_quiescing)
        is UpgradePhase.Draining -> stringResource(R.string.albums_upgrade_phase_draining)
        is UpgradePhase.Tombstoning -> stringResource(R.string.albums_upgrade_phase_tombstoning)
        is UpgradePhase.Forking -> stringResource(R.string.albums_upgrade_phase_forking)
        is UpgradePhase.Complete -> stringResource(R.string.albums_upgrade_phase_complete)
        is UpgradePhase.Aborted -> stringResource(R.string.albums_upgrade_phase_aborted)
    }

/**
 * The line under the phase: what is actually being waited on, or why it stopped.
 *
 * Queued writes are surfaced in every running phase on purpose. Quiescence holds the user's
 * changes locally rather than sending them, and a user who is not told that will reasonably
 * conclude their edits were lost.
 */
@Composable
internal fun upgradePhaseDetail(phase: UpgradePhase): String? =
    when (phase) {
        UpgradePhase.Idle, is UpgradePhase.Complete -> null
        is UpgradePhase.Quiescing -> queuedWrites(phase.queuedWrites)
        is UpgradePhase.Draining ->
            stringResource(R.string.albums_upgrade_in_flight).withArgs("count" to phase.inFlightUploads)

        is UpgradePhase.Tombstoning -> queuedWrites(phase.queuedWrites)
        is UpgradePhase.Forking -> queuedWrites(phase.queuedWrites)
        is UpgradePhase.Aborted -> abortReason(phase.reason)
    }

@Composable
private fun queuedWrites(count: Int): String {
    val template = stringResource(R.string.albums_upgrade_queued_writes)
    return template.withArgs("count" to count)
}

@Composable
private fun abortReason(reason: UpgradeAbortReason): String =
    stringResource(
        when (reason) {
            UpgradeAbortReason.DeadlineExpired -> R.string.albums_upgrade_abort_deadline
            UpgradeAbortReason.FrozenStateMismatch -> R.string.albums_upgrade_abort_hash
            UpgradeAbortReason.AbandonedByUser -> R.string.albums_upgrade_abort_user
        },
    )
