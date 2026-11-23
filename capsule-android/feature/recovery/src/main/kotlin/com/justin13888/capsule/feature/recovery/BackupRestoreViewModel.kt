package com.justin13888.capsule.feature.recovery

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.RecoveryRepository
import com.justin13888.capsule.core.model.BackupArtifactId
import com.justin13888.capsule.core.model.RestoreMode
import com.justin13888.capsule.core.model.RestoreReport
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The restore ladder's state machine: preview, then dry run, then commit.
 *
 * Every rung is a separate act by the user and every rung below the last writes nothing. The
 * ordering is enforced here rather than by the screen, because a ladder implemented in a
 * composable is a ladder that a second entry point steps over.
 *
 * Three refusals are structural rather than advisory:
 *
 * - An archive whose manifest MAC or exporter signature did not verify never reaches the dry
 *   run. Both authentications pass before anything is trusted, and an exporter device that has
 *   since been revoked is rejected — which is why the directory keeps a revoked device's public
 *   half forever.
 * - A commit is unreachable without a dry-run report, so what the user confirms is a report
 *   they have actually been shown.
 * - The typed confirmation is compared here. A confirmation checked in the UI is a
 *   confirmation the next caller forgets to check.
 *
 * What a commit applies is decided below this class: only assets absent locally are written.
 * A backup being older is never on its own a reason to roll a library back, so a live head
 * that has moved on, and an asset the user deleted after the export, both survive the commit
 * untouched and are surfaced for an explicit decision instead.
 */
class BackupRestoreViewModel(
    private val recovery: RecoveryRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(BackupRestoreUiState())

    /** Everything the screen draws. */
    val state: StateFlow<BackupRestoreUiState> = mutableState.asStateFlow()

    /** Edits where the archive is. Changing it drops everything read from the previous one. */
    fun onPathChange(value: String) {
        mutableState.value =
            BackupRestoreUiState(path = value, pathMissing = false)
    }

    /**
     * Reads the manifest and reports what the archive holds.
     *
     * Preview mode in the design's sense: shape only, no decryption, no writes. The manifest
     * is authenticated first, because everything downstream treats what it says as true.
     */
    fun preview() {
        val current = mutableState.value
        if (!current.canPreview) return
        if (current.path.isBlank()) {
            mutableState.update { it.copy(pathMissing = true) }
            return
        }
        mutableState.update { it.copy(busy = RestoreBusy.Previewing, failure = null, pathMissing = false) }
        viewModelScope.launch {
            runCatching {
                val artifact = recovery.inspectBackup(current.path.trim())
                val report =
                    if (artifact.isAuthenticated) {
                        recovery.restore(artifact.id, RestoreMode.Preview)
                    } else {
                        null
                    }
                artifact to report
            }.onSuccess { (artifact, report) ->
                mutableState.update {
                    it.copy(busy = null, stage = RestoreStage.Previewed, artifact = artifact, preview = report)
                }
            }.onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    /**
     * Verifies every entry and computes the diff against the live library. Writes nothing.
     *
     * Refused for an archive that did not authenticate: decrypting entries listed by a
     * manifest whose signature did not verify would be trusting the list that told us what to
     * decrypt.
     */
    fun dryRun() {
        val current = mutableState.value
        val artifact = current.artifact ?: return
        if (!current.canDryRun) return
        mutableState.update { it.copy(busy = RestoreBusy.DryRunning, failure = null) }
        runRestore(artifact.id, RestoreMode.DryRun) { report ->
            copy(busy = null, stage = RestoreStage.DryRun, report = report, merge = null)
        }
    }

    /** Moves to the typed confirmation, once a dry-run report has been read. */
    fun beginCommit() {
        if (mutableState.value.report == null) return
        mutableState.update { it.copy(stage = RestoreStage.Confirming, confirmation = "", confirmationWrong = false) }
    }

    /** Edits the typed confirmation. */
    fun onConfirmationChange(value: String) {
        mutableState.update { it.copy(confirmation = value, confirmationWrong = false) }
    }

    /**
     * Applies the dry run the user has read and confirmed.
     *
     * [expectedPhrase] is passed in rather than held here because it is a *localized* string:
     * a user typing the German word must be able to confirm, and a phrase hard-coded in this
     * class would silently demand English.
     */
    fun commit(expectedPhrase: String) {
        val current = mutableState.value
        val artifact = current.artifact ?: return
        if (!current.canConfirm) return
        if (!current.confirmation.trim().equals(expectedPhrase.trim(), ignoreCase = false)) {
            mutableState.update { it.copy(confirmationWrong = true) }
            return
        }
        mutableState.update { it.copy(busy = RestoreBusy.Committing, failure = null, confirmationWrong = false) }
        runRestore(artifact.id, RestoreMode.Commit) { report ->
            copy(busy = null, stage = RestoreStage.Committed, committed = report)
        }
    }

    /** Opens or closes the conflict-merge surface. */
    fun onReviewConflicts(reviewing: Boolean) {
        mutableState.update { it.copy(reviewingConflicts = reviewing) }
    }

    /** Records what the user decided about one conflicting asset. */
    fun onDecision(
        assetId: String,
        decision: RestoreDecision,
    ) {
        mutableState.update { it.copy(decisions = it.decisions + (assetId to decision), merge = null) }
    }

    /**
     * Turns the recorded decisions into a plan.
     *
     * Undecided means keep, so applying an untouched screen is a no-op rather than a surprise.
     * The plan is held rather than executed here: applying one asset's rollback is a per-asset
     * write the repository does not yet expose, and inventing a call that appeared to do it
     * would be the exact silent overwrite this whole flow exists to prevent.
     */
    fun applyMerge() {
        mutableState.update { current ->
            current.copy(merge = RestoreMerge.plan(current.conflicts, current.decisions), reviewingConflicts = false)
        }
    }

    /** Drops everything and starts from an empty path. */
    fun reset() {
        mutableState.value = BackupRestoreUiState()
    }

    private fun runRestore(
        id: BackupArtifactId,
        mode: RestoreMode,
        onReport: BackupRestoreUiState.(RestoreReport) -> BackupRestoreUiState,
    ) {
        viewModelScope.launch {
            runCatching { recovery.restore(id, mode) }
                .onSuccess { report -> mutableState.update { it.onReport(report) } }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    private fun BackupRestoreUiState.failed(throwable: Throwable): BackupRestoreUiState =
        copy(busy = null, failure = RecoveryFailure.of(throwable))
}
