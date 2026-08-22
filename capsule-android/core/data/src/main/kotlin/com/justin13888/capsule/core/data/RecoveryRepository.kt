package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.BackupArtifactId
import com.justin13888.capsule.core.model.BackupArtifactSummary
import com.justin13888.capsule.core.model.RecoveryState
import com.justin13888.capsule.core.model.RecoveryVerifyOutcome
import com.justin13888.capsule.core.model.RestoreMode
import com.justin13888.capsule.core.model.RestoreReport
import kotlinx.coroutines.flow.Flow
import java.time.Duration

/**
 * Proving the account is still recoverable, and the backup artifacts that make it so.
 *
 * [restore] takes a [RestoreMode] rather than defaulting to applying, because a restore that
 * silently overwrote current state would be indistinguishable from the stale-revival attack
 * the provenance chain exists to detect. The dry run is the normal path; committing is a
 * separate, deliberate act taken after its report has been read.
 */
interface RecoveryRepository {
    /** Where the user stands on having proved they can still recover. */
    fun recoveryState(): Flow<RecoveryState>

    /**
     * Checks an entered recovery secret against the escrow.
     *
     * Local and keyless — a derived-tag comparison with no network I/O — so a user can
     * confirm they still hold their secret on a plane. A stale cached blob is refreshed and
     * retried once before a failure is recorded.
     */
    suspend fun verifyRecoverySecret(secret: CharArray): RecoveryVerifyOutcome

    /**
     * Defers the next verification prompt.
     *
     * Suppresses the ask and nothing else. After the third consecutive snooze the prompt
     * becomes a persistent, non-blocking badge rather than disappearing — it never blocks
     * sync, unlock, or any other flow.
     */
    suspend fun snoozeVerification(by: Duration)

    /** Backup artifacts this client knows about. */
    fun backups(): Flow<List<BackupArtifactSummary>>

    /** Exports a new artifact to [destination], returning what it wrote. */
    suspend fun exportBackup(destination: String): BackupArtifactSummary

    /**
     * Reads an artifact's manifest without restoring anything.
     *
     * Both authentications must pass first — the keyed manifest MAC and the exporting
     * device's hybrid signature — and an exporter that has since been revoked is rejected.
     */
    suspend fun inspectBackup(path: String): BackupArtifactSummary

    /**
     * Reconciles an artifact against the live library under [mode].
     *
     * Only assets absent locally are applied automatically. A backup that is merely older
     * than the live copy is reported and left alone: "the backup is older" is never on its
     * own a reason to roll a library back.
     */
    suspend fun restore(
        id: BackupArtifactId,
        mode: RestoreMode = RestoreMode.DryRun,
    ): RestoreReport
}
