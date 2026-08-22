package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.EscrowError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.RecoveryRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockState
import com.justin13888.capsule.core.model.BackupArtifactId
import com.justin13888.capsule.core.model.BackupArtifactSummary
import com.justin13888.capsule.core.model.RecoveryCadence
import com.justin13888.capsule.core.model.RecoveryState
import com.justin13888.capsule.core.model.RecoveryVerifyOutcome
import com.justin13888.capsule.core.model.RestoreConflict
import com.justin13888.capsule.core.model.RestoreConflictKind
import com.justin13888.capsule.core.model.RestoreMode
import com.justin13888.capsule.core.model.RestoreReport
import com.justin13888.capsule.core.model.asWire
import kotlinx.coroutines.flow.Flow
import java.time.Duration
import java.time.Instant

/**
 * Proving the account is still recoverable, and the backup artifacts that make it so.
 *
 * [restore] reports identically in every mode and applies only in [RestoreMode.Commit], which
 * is the safety property rather than a convenience: a restore that silently overwrote current
 * state would be indistinguishable from the stale-revival attack the provenance chain exists
 * to detect. "The backup is older" is never on its own a reason to roll a library back, so
 * anything but an absent-locally asset is reported and left alone.
 */
class FakeRecoveryRepository(
    private val backend: MockBackend,
) : RecoveryRepository {
    override fun recoveryState(): Flow<RecoveryState> =
        backend.observe { state, _ ->
            // Derived on read rather than stored: this is a client-side cadence, not synced
            // state, so there is nothing here that another device could disagree with.
            deriveState(state, backend.now())
        }

    override suspend fun verifyRecoverySecret(secret: CharArray): RecoveryVerifyOutcome {
        // Local and keyless — a derived-tag comparison with no network I/O — so a user can
        // confirm they still hold their secret on a plane.
        backend.gate(MockOperation.RecoveryRead)
        val matches = String(secret).trim() == KNOWN_RECOVERY_SECRET
        backend.mutate { state ->
            if (matches) {
                state.recoveryLastVerifiedAt = backend.now()
                state.recoverySuccesses += 1
                state.recoveryFailures = 0
                state.recoverySnoozedUntil = null
                state.recoverySnoozes = 0
            } else {
                state.recoveryFailures += 1
            }
        }
        return if (matches) RecoveryVerifyOutcome.Verified else RecoveryVerifyOutcome.NotVerified
    }

    override suspend fun snoozeVerification(by: Duration) {
        backend.gate(MockOperation.RecoveryRead)
        // Suppresses the ask and nothing else. Past the third consecutive snooze the prompt
        // becomes a persistent badge rather than disappearing; it never blocks anything.
        backend.mutate { state ->
            state.recoverySnoozedUntil = backend.now().plus(by)
            state.recoverySnoozes += 1
        }
    }

    override fun backups(): Flow<List<BackupArtifactSummary>> = backend.observe { state, _ -> state.backups.toList() }

    override suspend fun exportBackup(destination: String): BackupArtifactSummary {
        backend.gate(MockOperation.RecoveryWrite)
        if (destination.isBlank()) throw EscrowError.Malformed
        val summary =
            backend.read { state -> state.backups.first() }.let { seed ->
                seed.copy(
                    id = BackupArtifactId(seed.id.value),
                    exportedAt = backend.now(),
                    entryCount = backend.read { it.assets.size },
                )
            }
        backend.mutate { state ->
            state.backups.clear()
            state.backups += summary
        }
        return summary
    }

    override suspend fun inspectBackup(path: String): BackupArtifactSummary {
        backend.gate(MockOperation.RecoveryRead)
        // Both authentications must pass before any of the manifest is trusted — the keyed MAC
        // and the exporting device's signature — and an exporter since revoked is rejected.
        if (path.isBlank()) throw EscrowError.Malformed
        return backend.read { it.backups.firstOrNull() } ?: throw LocalError.NotFound("backup at $path")
    }

    override suspend fun restore(
        id: BackupArtifactId,
        mode: RestoreMode,
    ): RestoreReport {
        backend.gate(MockOperation.RecoveryWrite)
        val artifact =
            backend.read { state -> state.backups.firstOrNull { it.id == id } }
                ?: throw LocalError.NotFound("backup ${id.value}")
        val live = backend.read { it.assets.values.toList() }
        val absent = live.takeLast(ABSENT_SAMPLE).map { it.id }
        val identical = live.take(artifact.entryCount - absent.size).map { it.id }
        val divergent =
            live.drop(identical.size).take(DIVERGENT_SAMPLE).map { asset ->
                RestoreConflict(
                    assetId = asset.id,
                    kind = RestoreConflictKind.Divergent.asWire(),
                    backupHead = asset.contentHash.value,
                    liveHead = asset.contentHash.value.reversed(),
                )
            }
        return RestoreReport(
            verified = artifact.entryCount,
            wouldAdd = absent,
            identical = identical,
            conflicts = divergent,
            // Only a commit writes, and even then only the assets absent locally.
            applied = if (mode.writes) absent else emptyList(),
        )
    }

    /** Where the user stands on having proved they can still recover the account. */
    private fun deriveState(
        state: MockState,
        now: Instant,
    ): RecoveryState {
        if (state.recoveryFailures >= RecoveryCadence.FAILURES_BEFORE_REWRAP) {
            // A guided re-wrap, not a lockout: the account is still reachable from this device,
            // and the point is to establish a secret the user does hold before that stops
            // being true.
            return RecoveryState.RewrapRecommended(state.recoveryFailures)
        }
        state.recoverySnoozedUntil?.let { until ->
            if (now.isBefore(until)) return RecoveryState.Snoozed(until, state.recoverySnoozes)
        }
        val lastVerified = state.recoveryLastVerifiedAt ?: return RecoveryState.NotConfigured
        val nextPrompt = lastVerified.plus(RecoveryCadence.intervalAfter(state.recoverySuccesses))
        return if (now.isBefore(nextPrompt)) {
            RecoveryState.Healthy(lastVerified, nextPrompt, state.recoverySuccesses)
        } else {
            RecoveryState.VerificationDue(nextPrompt)
        }
    }

    private companion object {
        /**
         * The phrase the fake accepts.
         *
         * Fixed rather than derived, because the mock holds no escrow blob to unwrap: the
         * value of the call to the UI is the shape of its answer, not the cryptography behind
         * it.
         */
        const val KNOWN_RECOVERY_SECRET = "correct horse battery staple"
        const val ABSENT_SAMPLE = 3
        const val DIVERGENT_SAMPLE = 2
    }
}
