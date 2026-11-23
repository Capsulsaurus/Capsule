package com.justin13888.capsule.feature.importer

import app.cash.turbine.test
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.UploadPolicy
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The plan-and-confirm state machine — where the free-space probe's three outcomes are
 * decided and where streaming's exclusion with staged uploads is enforced.
 *
 * Driven off the seeded fakes rather than hand-rolled doubles, so the plan under test is the
 * same deterministic plan the app would put in front of a user.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ImportPlanViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun installDispatcher() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun removeDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `a plan that fits is confirmable and carries its counts`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = conflictFreeStore(repositories, CANDIDATE_COUNT)
            val viewModel = viewModel(repositories, store, ROOMY_BYTES)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertFalse(loaded.isLoading)
                assertNotNull(loaded.plan)
                val plan = loaded.plan!!
                assertEquals(CANDIDATE_COUNT, plan.added.size + plan.skipped.size)
                assertEquals(ImportSpaceVerdict.Fits, loaded.space)
                assertTrue(loaded.canConfirm)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a plan larger than free space recommends streaming`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = conflictFreeStore(repositories, CANDIDATE_COUNT)
            val viewModel = viewModel(repositories, store, store.only().plan.totalSize)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertTrue(loaded.isStreaming)
                assertEquals(true, loaded.plan?.streamingRecommended)
                assertTrue(loaded.canConfirm)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `one file larger than the disk is a hard refusal, not a streaming suggestion`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = conflictFreeStore(repositories, CANDIDATE_COUNT)
            val largest =
                store
                    .only()
                    .plan.added
                    .maxOf { it.sizeBytes }
            val viewModel = viewModel(repositories, store, largest - 1)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                val space = loaded.space
                assertTrue(space is ImportSpaceVerdict.SingleFileTooLarge)
                assertEquals(1L, (space as ImportSpaceVerdict.SingleFileTooLarge).shortfallBytes)
                assertTrue(loaded.isBlocked)
                assertFalse(loaded.canConfirm)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `streaming refuses to run under a staged upload policy until it is switched`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            repositories.sync.setUploadPolicy(UploadPolicy.Staged)
            val store = conflictFreeStore(repositories, CANDIDATE_COUNT)
            val viewModel = viewModel(repositories, store, store.only().plan.totalSize)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val refused = expectMostRecentItem()
                assertTrue(refused.hasStagedConflict)
                assertFalse(refused.canConfirm)

                viewModel.switchToFullUploads()
                advanceUntilIdle()
                val resolved = expectMostRecentItem()

                assertFalse(resolved.hasStagedConflict)
                assertTrue(resolved.canConfirm)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `confirming starts a run and reports the id the shell routes on`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = conflictFreeStore(repositories, CANDIDATE_COUNT)
            val viewModel = viewModel(repositories, store, ROOMY_BYTES)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()
                viewModel.confirm()
                advanceUntilIdle()

                assertEquals(loaded.plan?.importId, expectMostRecentItem().startedImportId)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a refused confirmation surfaces the code the user can act on`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = conflictFreeStore(repositories, CANDIDATE_COUNT)
            repositories.applyScenario(MockScenario.QuotaExhausted)
            val viewModel = viewModel(repositories, store, ROOMY_BYTES)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.confirm()
                advanceUntilIdle()
                val refused = expectMostRecentItem()

                assertEquals("error.quota.exceeded", refused.failure?.value)
                assertNull(refused.startedImportId)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a plan this process never computed fails locally rather than guessing`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val viewModel =
                ImportPlanViewModel(
                    imports = repositories.imports,
                    albums = repositories.albums,
                    sync = repositories.sync,
                    freeSpace = ImporterFixture.freeSpace(ROOMY_BYTES),
                    plans = InMemoryImportPlanStore(),
                    importId = ImportId("never-planned"),
                )

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val failed = expectMostRecentItem()

                assertTrue(failed.hasLocalFailure)
                assertTrue(failed.hasFailed)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a planner that cannot run leaves no plan and an error state`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = conflictFreeStore(repositories, CANDIDATE_COUNT)
            repositories.applyScenario(
                MockScenario.Default.failing(MockOperation.ImportPlan, LocalError.Offline),
            )
            val viewModel = viewModel(repositories, store, ROOMY_BYTES)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val failed = expectMostRecentItem()

                assertTrue(failed.hasFailed)
                assertNull(failed.plan)
                cancelAndIgnoreRemainingEvents()
            }
        }

    private fun viewModel(
        repositories: MockRepositories,
        store: RecordingPlanStore,
        freeBytes: Long,
    ) = ImportPlanViewModel(
        imports = repositories.imports,
        albums = repositories.albums,
        sync = repositories.sync,
        freeSpace = ImporterFixture.freeSpace(freeBytes),
        plans = store,
        importId = store.only().plan.importId,
    )

    /**
     * A store holding a plan the planner raised no questions about.
     *
     * The fixture's classifier turns a fraction of any locator set into conflicts, and a plan
     * with conflicts is deliberately not confirmable — so a test about free space would
     * otherwise be asserting the conflict rule by accident.
     */
    private suspend fun conflictFreeStore(
        repositories: MockRepositories,
        count: Int,
    ): RecordingPlanStore {
        val pool = (1..count * POOL_FACTOR).map { "DCIM/Camera/IMG_$it.JPG" }
        val probe = repositories.imports.plan(pool, destination = null)
        val conflicting = probe.conflicts.map { it.sourceLocator }.toSet()
        val usable = pool.filterNot { it in conflicting }.take(count)
        val store = RecordingPlanStore()
        store.put(PlannedImport(repositories.imports.plan(usable, destination = null), usable, destination = null))
        return store
    }

    /** A store that also answers "which plan is in here", so a test need not repeat the id. */
    private class RecordingPlanStore : ImportPlanStore {
        private val delegate = InMemoryImportPlanStore()
        private var last: PlannedImport? = null

        override fun put(planned: PlannedImport) {
            last = planned
            delegate.put(planned)
        }

        override fun get(id: ImportId): PlannedImport? = delegate.get(id)

        fun only(): PlannedImport = requireNotNull(last) { "no plan recorded" }
    }

    private companion object {
        /** Enough files that the counts are worth asserting, few enough to stay quick. */
        const val CANDIDATE_COUNT = 12

        /** How much larger the probe pool is than the plan, so conflicts can be dropped. */
        const val POOL_FACTOR = 4

        /** More free space than any fixture plan can ask for. */
        const val ROOMY_BYTES = 1_000_000_000_000L
    }
}
