package com.justin13888.capsule.feature.storage

import app.cash.turbine.test
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.QuotaState
import com.justin13888.capsule.core.model.SyncScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
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

/**
 * The state machine behind both storage destinations.
 *
 * The release path gets the most attention here because it is the only path in this feature
 * that can lose something: everything else either reads, or discards bytes that are
 * rebuildable by construction.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class StorageViewModelTest {
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
    fun `it opens loading and settles on a reading of both authorities`() =
        runTest(dispatcher) {
            val viewModel = viewModelOver(MockRepositories.from(StorageFixtures.backend()))

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the first state is loading, before anything is read")
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertNull(settled.failure)
                assertNotNull(settled.standing, "the account's standing arrived")
                assertTrue(settled.cacheBytes > 0, "the device is holding something")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the consumers are the heaviest first so the list answers what to clear`() =
        runTest(dispatcher) {
            val consumers = settled(MockScenario.Default).state.value.consumers

            assertTrue(consumers.isNotEmpty(), "the seeded library holds representations locally")
            assertEquals(consumers.sortedByDescending(StorageConsumer::bytes), consumers)
            assertTrue(
                consumers.all { it.held.best.rank > 0 },
                "an asset holding only the placeholder can free nothing and is not listed",
            )
        }

    @Test
    fun `each documented quota state reaches the screen as its own standing`() =
        runTest(dispatcher) {
            val expected =
                listOf(
                    MockScenario.QuotaSoftWarning to QuotaState.SoftWarning,
                    MockScenario.QuotaExhausted to QuotaState.HardExceeded,
                    MockScenario.QuotaGraceExpired to QuotaState.GraceExpired,
                    MockScenario.AccountSuspended to QuotaState.Suspended,
                )
            expected.forEach { (scenario, state) ->
                assertEquals(
                    state,
                    settled(scenario)
                        .state.value.standing
                        ?.state,
                    scenario.id,
                )
            }
        }

    @Test
    fun `a reclamation reports what it freed`() =
        runTest(dispatcher) {
            val viewModel = settled(MockScenario.Default)
            viewModel.setBudget(CacheBudget.Small)

            viewModel.reclaim()
            advanceUntilIdle()

            assertTrue(
                viewModel.state.value.reclaim is ReclaimOutcome.Freed,
                "a finished sweep reports a figure rather than leaving a spinner",
            )
            assertFalse(viewModel.state.value.isReclaiming)
        }

    @Test
    fun `a reclamation that cannot run surfaces the refusal rather than a false result`() =
        runTest(dispatcher) {
            val viewModel = settled(MockScenario.Offline)

            viewModel.reclaim()
            advanceUntilIdle()

            assertNull(viewModel.state.value.reclaim, "a refused sweep is not reported as a result")
            assertNotNull(viewModel.state.value.failure, "the refusal is still surfaced")
            assertNull(
                viewModel.state.value.failure
                    ?.code,
                "a purely local refusal has no catalogue code to invent",
            )
        }

    @Test
    fun `pinning is recorded so the row can say it is kept offline`() =
        runTest(dispatcher) {
            val viewModel = settled(MockScenario.Default)
            val target =
                viewModel.state.value.consumers
                    .first()
                    .id

            viewModel.setPinned(target, true)
            advanceUntilIdle()

            assertTrue(
                viewModel.state.value
                    .consumer(target.value)
                    ?.isPinned == true,
            )
            viewModel.setPinned(target, false)
            advanceUntilIdle()
            assertFalse(
                viewModel.state.value
                    .consumer(target.value)
                    ?.isPinned == true,
            )
        }

    @Test
    fun `a durable verdict releases the original and says it can come back`() =
        runTest(dispatcher) {
            val (viewModel, target) = withHeldOriginal(MockScenario.Default)

            viewModel.releaseOriginal(target)
            advanceUntilIdle()

            assertEquals(
                ReleaseState.Released,
                viewModel.state.value
                    .consumer(target.value)
                    ?.release,
            )
        }

    @Test
    fun `an unconfirmed verdict keeps the bytes and shows the unconfirmed state`() =
        runTest(dispatcher) {
            val backend = StorageFixtures.backend()
            val repositories = MockRepositories.from(backend)
            val target = StorageFixtures.holdOriginal(repositories)
            val viewModel = viewModelOver(repositories)
            advanceUntilIdle()
            // The library's originals have not left their capturing device in this scenario,
            // so the durability check is honestly unable to say yes.
            backend.applyScenario(MockScenario.AwaitingOriginal)
            advanceUntilIdle()

            viewModel.releaseOriginal(target)
            advanceUntilIdle()

            assertEquals(
                ReleaseState.Unconfirmed,
                viewModel.state.value
                    .consumer(target.value)
                    ?.release,
            )
        }

    @Test
    fun `a check that cannot be answered is never read as a yes`() =
        runTest(dispatcher) {
            val backend = StorageFixtures.backend()
            val repositories = MockRepositories.from(backend)
            val target = StorageFixtures.holdOriginal(repositories)
            val viewModel = viewModelOver(repositories)
            advanceUntilIdle()
            backend.applyScenario(MockScenario.Default.failing(MockOperation.StorageWrite, LocalError.Offline))
            advanceUntilIdle()

            viewModel.releaseOriginal(target)
            advanceUntilIdle()

            assertEquals(
                ReleaseState.Unconfirmed,
                viewModel.state.value
                    .consumer(target.value)
                    ?.release,
                "an unanswered question is not a durable verdict",
            )
        }

    @Test
    fun `a coded failure on the check is reported alongside the kept bytes`() =
        runTest(dispatcher) {
            val backend = StorageFixtures.backend()
            val repositories = MockRepositories.from(backend)
            val target = StorageFixtures.holdOriginal(repositories)
            val viewModel = viewModelOver(repositories)
            advanceUntilIdle()
            backend.applyScenario(MockScenario.Default.failing(MockOperation.StorageWrite, QuotaError.Exceeded))
            advanceUntilIdle()

            viewModel.releaseOriginal(target)
            advanceUntilIdle()

            assertEquals(
                ReleaseState.Unconfirmed,
                viewModel.state.value
                    .consumer(target.value)
                    ?.release,
            )
            assertEquals(
                QuotaError.Exceeded.code,
                viewModel.state.value.failure
                    ?.code,
                "a coded refusal is localizable and therefore worth surfacing",
            )
        }

    @Test
    fun `lowering the sync scope is written through and releases nothing`() =
        runTest(dispatcher) {
            val repositories = MockRepositories.from(StorageFixtures.backend())
            val viewModel = viewModelOver(repositories)
            advanceUntilIdle()
            val before = viewModel.state.value.cacheBytes

            viewModel.setSyncScope(SyncScope.MetadataOnly)
            advanceUntilIdle()

            assertEquals(SyncScope.MetadataOnly, repositories.sync.syncScope().first())
            assertEquals(before, viewModel.state.value.cacheBytes, "lowering the scope evicts nothing")
        }

    @Test
    fun `a failed reading offers a retry that actually restarts the collection`() =
        runTest(dispatcher) {
            val backend =
                StorageFixtures.backend(
                    MockScenario.Default.failing(MockOperation.TimelineRead, LocalError.Offline),
                )
            val viewModel = viewModelOver(MockRepositories.from(backend))
            advanceUntilIdle()
            assertNotNull(viewModel.state.value.failure, "the reading failed")

            backend.applyScenario(MockScenario.Default)
            viewModel.reload()
            advanceUntilIdle()

            assertNull(viewModel.state.value.failure)
            assertNotNull(viewModel.state.value.standing)
        }

    private fun viewModelOver(repositories: MockRepositories): StorageViewModel =
        StorageViewModel(
            source =
                StorageSource(
                    storage = repositories.storage,
                    sync = repositories.sync,
                    timeline = repositories.timeline,
                ),
            storage = repositories.storage,
            sync = repositories.sync,
            clock = StorageFixtures.clock(),
        )

    private fun TestScope.settled(scenario: MockScenario): StorageViewModel {
        val viewModel = viewModelOver(MockRepositories.from(StorageFixtures.backend(scenario)))
        advanceUntilIdle()
        return viewModel
    }

    private suspend fun TestScope.withHeldOriginal(scenario: MockScenario): Pair<StorageViewModel, AssetId> {
        val repositories = MockRepositories.from(StorageFixtures.backend(scenario))
        val target = StorageFixtures.holdOriginal(repositories)
        val viewModel = viewModelOver(repositories)
        advanceUntilIdle()
        return viewModel to target
    }
}
