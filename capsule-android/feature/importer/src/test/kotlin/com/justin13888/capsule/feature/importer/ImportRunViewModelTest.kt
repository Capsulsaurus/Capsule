package com.justin13888.capsule.feature.importer

import app.cash.turbine.test
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.ImportFileState
import com.justin13888.capsule.core.model.ImportId
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import java.time.Duration

/**
 * The execution screen's state machine, driven over a virtual clock.
 *
 * The fixture's `confirm` lands finished because it moves no bytes, so the per-file ladder is
 * the view model's own — and that is exactly what these assert: that a run really does pass
 * through processing, encrypting and uploading, that a streaming run holds only its window
 * on the device, and that losing the server pauses it rather than letting it run ahead.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ImportRunViewModelTest {
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
    fun `a run walks its files through every rung of the pipeline`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = startedRun(repositories, streaming = false)
            val viewModel = runViewModel(repositories, store)

            viewModel.state.test {
                val attached = awaitItem()
                runCurrent()
                assertTrue(attached.progression.rows.all { it.state == ImportFileState.Queued })

                advanceTimeBy(TICK.toMillis() + 1)
                assertEquals(ImportFileState.Processing, expectMostRecentItem().inFlight.first().state)

                advanceTimeBy(TICK.toMillis())
                assertEquals(ImportFileState.Encrypting, expectMostRecentItem().inFlight.first().state)

                advanceTimeBy(TICK.toMillis())
                assertEquals(ImportFileState.Uploading, expectMostRecentItem().inFlight.first().state)

                advanceTimeBy(TICK.toMillis())
                assertEquals(
                    ImportFileState.Done,
                    expectMostRecentItem()
                        .progression.rows
                        .first()
                        .state,
                )
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a streaming run keeps only its window on the device`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = startedRun(repositories, streaming = true)
            val viewModel = runViewModel(repositories, store)

            viewModel.state.test {
                val attached = awaitItem()
                runCurrent()
                assertTrue(attached.isStreaming)

                advanceTimeBy(TICK.toMillis() * 2 + 1)
                val running = expectMostRecentItem()

                assertEquals(ImportProgression.STREAMING_WINDOW, running.inFlight.size)
                val untouched = running.progression.rows.drop(ImportProgression.STREAMING_WINDOW)
                assertTrue(untouched.all { it.state == ImportFileState.Queued })
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `losing the server pauses a streaming run rather than importing ahead`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = startedRun(repositories, streaming = true)
            val viewModel = runViewModel(repositories, store)

            viewModel.state.test {
                awaitItem()
                runCurrent()
                advanceTimeBy(TICK.toMillis() + 1)
                val running = expectMostRecentItem()
                assertFalse(running.isPaused)

                repositories.applyScenario(MockScenario.Offline)
                runCurrent()
                val paused = expectMostRecentItem()
                assertTrue(paused.isPaused)

                advanceTimeBy(TICK.toMillis() * 4)
                expectNoEvents()
                assertEquals(paused.progression.rows, viewModel.state.value.progression.rows)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a non-streaming run carries on while offline`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories(MockScenario.Offline)
            val store = startedRun(repositories, streaming = false)
            val viewModel = runViewModel(repositories, store)

            viewModel.state.test {
                awaitItem()
                runCurrent()
                advanceTimeBy(TICK.toMillis() + 1)
                val running = expectMostRecentItem()

                assertFalse(running.isPaused)
                assertEquals(ImportFileState.Processing, running.inFlight.first().state)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `cancelling keeps what finished and shows the summary`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val store = startedRun(repositories, streaming = false)
            val viewModel = runViewModel(repositories, store)

            viewModel.state.test {
                awaitItem()
                runCurrent()
                advanceTimeBy(TICK.toMillis() * 4 + 1)
                val partway = expectMostRecentItem()
                assertTrue(partway.progression.completed > 0)

                viewModel.cancel()
                runCurrent()
                val stopped = expectMostRecentItem()

                assertTrue(stopped.isCancelled)
                assertTrue(stopped.showsSummary)
                assertEquals(partway.progression.completed, stopped.progression.total)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a run this process cannot name fails locally rather than guessing`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val viewModel =
                ImportRunViewModel(
                    imports = repositories.imports,
                    sync = repositories.sync,
                    plans = InMemoryImportPlanStore(),
                    importId = ImportId("never-planned"),
                    tick = TICK,
                )

            viewModel.state.test {
                val failed = awaitItem()

                assertTrue(failed.hasLocalFailure)
                assertTrue(failed.hasFailed)
                cancelAndIgnoreRemainingEvents()
            }
        }

    private fun runViewModel(
        repositories: MockRepositories,
        store: ImportPlanStore,
    ): ImportRunViewModel {
        val planned = requireNotNull((store as SingleRunStore).recorded)
        return ImportRunViewModel(
            imports = repositories.imports,
            sync = repositories.sync,
            plans = store,
            importId = planned.plan.importId,
            tick = TICK,
        )
    }

    /**
     * A confirmed run whose plan is conflict-free, marked streaming or not.
     *
     * `streamingRecommended` is set here because it is what the run screen reads to choose its
     * window — the same field confirmation attaches from the free-space probe.
     */
    private suspend fun startedRun(
        repositories: MockRepositories,
        streaming: Boolean,
    ): SingleRunStore {
        val pool = (1..FILE_COUNT * POOL_FACTOR).map { "DCIM/Camera/IMG_$it.JPG" }
        val probe = repositories.imports.plan(pool, destination = null)
        val conflicting = probe.conflicts.map { it.sourceLocator }.toSet()
        val usable = pool.filterNot { it in conflicting }.take(FILE_COUNT)
        val plan = repositories.imports.plan(usable, destination = null).copy(streamingRecommended = streaming)
        val store = SingleRunStore()
        store.put(PlannedImport(plan, usable, destination = null))
        return store
    }

    /** A store holding exactly one plan, so a test need not repeat its id. */
    private class SingleRunStore : ImportPlanStore {
        private val delegate = InMemoryImportPlanStore()

        /** The plan this store was given. */
        var recorded: PlannedImport? = null
            private set

        override fun put(planned: PlannedImport) {
            recorded = planned
            delegate.put(planned)
        }

        override fun get(id: ImportId): PlannedImport? = delegate.get(id)
    }

    private companion object {
        /** A short, exact tick — virtual, so the suite waits for none of it. */
        val TICK: Duration = Duration.ofMillis(10)

        /** Enough files that the window bound is observable. */
        const val FILE_COUNT = 12

        /** How much larger the probe pool is than the plan, so conflicts can be dropped. */
        const val POOL_FACTOR = 4
    }
}
