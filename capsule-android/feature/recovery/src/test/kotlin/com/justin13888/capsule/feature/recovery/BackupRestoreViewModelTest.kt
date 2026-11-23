package com.justin13888.capsule.feature.recovery

import com.justin13888.capsule.core.data.EscrowError
import com.justin13888.capsule.core.data.RecoveryRepository
import com.justin13888.capsule.core.model.BackupArtifactId
import com.justin13888.capsule.core.model.BackupArtifactSummary
import com.justin13888.capsule.core.model.RecoveryState
import com.justin13888.capsule.core.model.RecoveryVerifyOutcome
import com.justin13888.capsule.core.model.RestoreMode
import com.justin13888.capsule.core.model.RestoreReport
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test
import java.time.Duration

/**
 * The restore ladder.
 *
 * This is the dangerous flow in the whole feature, so the cases are the refusals rather than
 * the happy path: nothing writes before a commit, a commit is unreachable without a confirmed
 * dry-run report, an unauthenticated archive never gets decrypted, and a conflict is never
 * applied on the strength of the archive being older.
 *
 * The recording repository below is deliberate rather than a shortcut. The assertion that
 * matters is *which modes were asked for, in which order* — a fact the seeded fake cannot be
 * interrogated about, and the fact that decides whether this screen can destroy a library.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class BackupRestoreViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun installMainDispatcher() {
        Dispatchers.setMain(dispatcher)
    }

    @AfterEach
    fun removeMainDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `it opens with nothing read and nothing offered`() =
        runTest(dispatcher) {
            val viewModel = BackupRestoreViewModel(RecordingRecoveryRepository())

            val state = viewModel.state.value
            assertEquals(RestoreStage.Choosing, state.stage)
            assertNull(state.artifact)
            assertFalse(state.canDryRun, "there is nothing to dry-run against")
            assertFalse(state.canConfirm)
        }

    @Test
    fun `naming no archive says so rather than reading nothing`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository()
            val viewModel = BackupRestoreViewModel(recorder)

            viewModel.preview()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.pathMissing)
            assertTrue(recorder.modes.isEmpty(), "nothing was asked of the repository")
        }

    @Test
    fun `a preview reads the manifest in preview mode and writes nothing`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository()
            val viewModel = previewed(recorder)

            assertEquals(RestoreStage.Previewed, viewModel.state.value.stage)
            assertEquals(listOf(RestoreMode.Preview), recorder.modes)
            assertTrue(recorder.modes.none(RestoreMode::writes))
            assertNotNull(viewModel.state.value.artifact)
        }

    @Test
    fun `an archive that did not authenticate is never decrypted or reconciled`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository(artifact = RecoveryFixtures.UNAUTHENTICATED)
            val viewModel = BackupRestoreViewModel(recorder)
            viewModel.onPathChange(RecoveryFixtures.ARCHIVE_PATH)
            viewModel.preview()
            advanceUntilIdle()

            viewModel.dryRun()
            advanceUntilIdle()

            assertFalse(viewModel.state.value.isAuthenticated)
            assertTrue(recorder.modes.isEmpty(), "not even a preview decrypt of a manifest we cannot trust")
            assertNull(viewModel.state.value.report)
        }

    @Test
    fun `a dry run reports the diff and still writes nothing`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository()
            val viewModel = dryRun(recorder)

            val state = viewModel.state.value
            assertEquals(RestoreStage.DryRun, state.stage)
            assertEquals(listOf(RestoreMode.Preview, RestoreMode.DryRun), recorder.modes)
            assertTrue(recorder.modes.none(RestoreMode::writes))
            assertEquals(2, state.conflicts.size)
            assertEquals(1, state.wouldAddCount)
        }

    @Test
    fun `a commit is unreachable without a dry-run report to confirm`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository()
            val viewModel = previewed(recorder)

            viewModel.beginCommit()
            viewModel.onConfirmationChange(PHRASE)
            viewModel.commit(PHRASE)
            advanceUntilIdle()

            assertEquals(RestoreStage.Previewed, viewModel.state.value.stage)
            assertEquals(listOf(RestoreMode.Preview), recorder.modes, "no commit without a report")
        }

    @Test
    fun `a mistyped confirmation refuses the commit rather than proceeding`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository()
            val viewModel = dryRun(recorder)
            viewModel.beginCommit()

            viewModel.onConfirmationChange("restore")
            viewModel.commit(PHRASE)
            advanceUntilIdle()

            assertTrue(viewModel.state.value.confirmationWrong, "the phrase is compared exactly, case included")
            assertFalse(recorder.modes.any(RestoreMode::writes))
        }

    @Test
    fun `the confirmed commit applies only what was missing locally`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository()
            val viewModel = dryRun(recorder)
            viewModel.beginCommit()

            viewModel.onConfirmationChange(PHRASE)
            viewModel.commit(PHRASE)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(RestoreStage.Committed, state.stage)
            assertEquals(listOf(RestoreMode.Preview, RestoreMode.DryRun, RestoreMode.Commit), recorder.modes)
            val applied = state.committed?.applied.orEmpty()
            assertEquals(recorder.report.wouldAdd, applied, "only absent-locally assets are written")
            assertTrue(
                state.conflicts.none { it.assetId in applied },
                "a conflicting asset must never be written by a commit",
            )
        }

    @Test
    fun `an asset deleted after the export is not resurrected by the commit`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository()
            val viewModel = dryRun(recorder)
            viewModel.beginCommit()
            viewModel.onConfirmationChange(PHRASE)

            viewModel.commit(PHRASE)
            advanceUntilIdle()

            assertFalse(
                RecoveryFixtures.DIVERGENT.assetId in
                    viewModel.state.value.committed
                        ?.applied
                        .orEmpty(),
                "a six-month-old archive cannot bring back what the user deleted last week",
            )
        }

    @Test
    fun `merge decisions default to keeping what is here`() =
        runTest(dispatcher) {
            val viewModel = dryRun(RecordingRecoveryRepository())

            viewModel.applyMerge()

            val plan = viewModel.state.value.merge
            assertNotNull(plan)
            assertTrue(plan!!.rolledBack.isEmpty(), "an untouched merge screen changes nothing")
            assertEquals(2, plan.keptLocal.size)
        }

    @Test
    fun `an explicit choice of the older copy is recorded as a rollback`() =
        runTest(dispatcher) {
            val viewModel = dryRun(RecordingRecoveryRepository())

            viewModel.onDecision(RecoveryFixtures.NEWER_LOCALLY.assetId.value, RestoreDecision.UseBackup)
            viewModel.applyMerge()

            assertEquals(
                listOf(RecoveryFixtures.NEWER_LOCALLY.assetId),
                viewModel.state.value.merge
                    ?.rolledBack,
            )
            assertFalse(viewModel.state.value.reviewingConflicts, "applying returns to the report")
        }

    @Test
    fun `changing the archive drops everything read from the previous one`() =
        runTest(dispatcher) {
            val viewModel = dryRun(RecordingRecoveryRepository())

            viewModel.onPathChange("/sdcard/Download/other.tar")

            val state = viewModel.state.value
            assertEquals(RestoreStage.Choosing, state.stage)
            assertNull(state.artifact)
            assertNull(state.report)
        }

    @Test
    fun `a failure surfaces its catalogue code`() =
        runTest(dispatcher) {
            val recorder = RecordingRecoveryRepository(inspectFailure = EscrowError.Malformed)
            val viewModel = BackupRestoreViewModel(recorder)
            viewModel.onPathChange(RecoveryFixtures.ARCHIVE_PATH)

            viewModel.preview()
            advanceUntilIdle()

            assertEquals(
                EscrowError.Malformed.code,
                viewModel.state.value.failure
                    ?.code,
            )
            assertNull(viewModel.state.value.busy)
        }

    private fun TestScope.previewed(recorder: RecordingRecoveryRepository): BackupRestoreViewModel =
        BackupRestoreViewModel(recorder).also {
            it.onPathChange(RecoveryFixtures.ARCHIVE_PATH)
            it.preview()
            advanceUntilIdle()
        }

    private fun TestScope.dryRun(recorder: RecordingRecoveryRepository): BackupRestoreViewModel =
        previewed(recorder).also {
            it.dryRun()
            advanceUntilIdle()
        }

    @Test
    fun `the whole ladder over the seeded library leaves every conflict untouched`() =
        runTest(dispatcher) {
            // The same walk against the real fake rather than the recorder, so the ordering
            // assertions above are known to describe a repository that actually behaves this way.
            val viewModel = BackupRestoreViewModel(RecoveryFixtures.repositories().recovery)
            viewModel.onPathChange(RecoveryFixtures.ARCHIVE_PATH)
            viewModel.preview()
            advanceUntilIdle()
            viewModel.dryRun()
            advanceUntilIdle()
            viewModel.beginCommit()
            viewModel.onConfirmationChange(PHRASE)

            viewModel.commit(PHRASE)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(RestoreStage.Committed, state.stage)
            // What the commit wrote is exactly what the dry run said it would, which is the
            // property "what you confirmed is what happened" rests on. The stronger claim —
            // that a conflicting asset is never among them — is asserted against the recorder
            // above, because the seeded fake draws its conflicts from the same tail of the
            // library as its absent-locally sample and so cannot distinguish the two.
            assertEquals(state.report?.wouldAdd, state.committed?.applied)
            assertNotNull(state.report)
            assertTrue(state.report!!.verified > 0, "a commit that verified nothing verified nothing")
        }

    private companion object {
        /** The English confirmation phrase, which is what the catalogue holds for this locale. */
        const val PHRASE = "RESTORE"
    }
}

/**
 * A repository that remembers which restore modes it was asked for.
 *
 * Every other method throws or answers trivially, because this test is about one question and
 * a double that answered more would invite tests that asked about something else.
 */
private class RecordingRecoveryRepository(
    private val artifact: BackupArtifactSummary = RecoveryFixtures.ARTIFACT,
    private val inspectFailure: Throwable? = null,
    val report: RestoreReport = RecoveryFixtures.report(),
) : RecoveryRepository {
    /** Every mode asked for, in order. */
    val modes = mutableListOf<RestoreMode>()

    override fun recoveryState(): Flow<RecoveryState> = flowOf(RecoveryState.NotConfigured)

    override suspend fun verifyRecoverySecret(secret: CharArray): RecoveryVerifyOutcome = MISSED

    override suspend fun snoozeVerification(by: Duration) = Unit

    override fun backups(): Flow<List<BackupArtifactSummary>> = flowOf(listOf(artifact))

    override suspend fun exportBackup(destination: String): BackupArtifactSummary = artifact

    override suspend fun inspectBackup(path: String): BackupArtifactSummary {
        inspectFailure?.let { throw it }
        return artifact
    }

    override suspend fun restore(
        id: BackupArtifactId,
        mode: RestoreMode,
    ): RestoreReport {
        modes += mode
        return if (mode.writes) report.copy(applied = report.wouldAdd) else report
    }
}

/** What the recorder answers a verification with. It is asked, and it is never the subject. */
private val MISSED = RecoveryVerifyOutcome.NotVerified
