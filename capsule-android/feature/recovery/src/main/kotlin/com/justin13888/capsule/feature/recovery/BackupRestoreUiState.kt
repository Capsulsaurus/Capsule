package com.justin13888.capsule.feature.recovery

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.BackupArtifactSummary
import com.justin13888.capsule.core.model.RestoreReport

/** How far up the three-mode ladder the user has climbed. */
enum class RestoreStage {
    /** No archive named yet. */
    Choosing,

    /** The manifest has been read. Nothing decrypted, nothing written. */
    Previewed,

    /** The diff against the live library has been computed. Still nothing written. */
    DryRun,

    /** The typed confirmation is being asked for. */
    Confirming,

    /** The commit ran. */
    Committed,
}

/** Which call is in flight, if any. */
enum class RestoreBusy {
    /** Reading and authenticating the manifest. */
    Previewing,

    /** Verifying and reconciling. */
    DryRunning,

    /** Applying. */
    Committing,
}

/**
 * Everything the restore screen draws, as one value.
 *
 * The stage only ever advances by an explicit act, and every stage below [RestoreStage.Committed]
 * has written nothing. That is not a convention this state hopes callers honour — the commit
 * is unreachable without a typed confirmation, and the confirmation is unreachable without a
 * dry-run report, so the ladder is the type rather than a sequence of hopes.
 */
@Immutable
data class BackupRestoreUiState(
    /** Where the archive is. */
    val path: String = "",
    /** Whether the last attempt named nowhere. */
    val pathMissing: Boolean = false,
    /** How far up the ladder the user has climbed. */
    val stage: RestoreStage = RestoreStage.Choosing,
    /** Which call is in flight. */
    val busy: RestoreBusy? = null,
    /** What the manifest says the archive holds. */
    val artifact: BackupArtifactSummary? = null,
    /** What a preview verified, without decrypting anything. */
    val preview: RestoreReport? = null,
    /** What a restore would do. */
    val report: RestoreReport? = null,
    /** What a commit actually did. */
    val committed: RestoreReport? = null,
    /** What the user has typed into the confirmation field. */
    val confirmation: String = "",
    /** Whether the last commit attempt was refused for a mistyped confirmation. */
    val confirmationWrong: Boolean = false,
    /** What the user has said about each conflicting asset. */
    val decisions: Map<String, RestoreDecision> = emptyMap(),
    /** Whether the conflict-merge surface is on top. */
    val reviewingConflicts: Boolean = false,
    /** What the recorded decisions would do. */
    val merge: RestoreMergePlan? = null,
    /** The failure to explain, or `null`. */
    val failure: RecoveryFailure? = null,
) {
    /** Whether the archive authenticated. A restore of one that did not is refused outright. */
    val isAuthenticated: Boolean get() = artifact?.isAuthenticated == true

    /** The conflicts the dry run found, which is the only place they come from. */
    val conflicts get() = report?.conflicts.orEmpty()

    /** How many entries a commit would actually write. */
    val wouldAddCount: Int get() = report?.wouldAdd?.size ?: 0

    /** How many conflicts nobody has decided about. */
    val undecidedCount: Int get() = RestoreMerge.undecidedCount(conflicts, decisions)

    /** Whether the preview action is live. */
    val canPreview: Boolean get() = busy == null

    /** Whether the dry run can be run. */
    val canDryRun: Boolean get() = busy == null && isAuthenticated && artifact != null

    /** Whether the commit can even be offered. */
    val canConfirm: Boolean get() = busy == null && report != null && isAuthenticated
}
