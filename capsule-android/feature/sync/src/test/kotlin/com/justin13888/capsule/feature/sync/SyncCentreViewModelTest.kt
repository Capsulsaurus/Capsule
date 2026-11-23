package com.justin13888.capsule.feature.sync

import app.cash.turbine.test
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.QuarantineSurface
import com.justin13888.capsule.core.model.StaleSyncNotification
import com.justin13888.capsule.core.model.SyncScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import java.time.Duration
import java.time.Instant

/**
 * The sync centre's state machine.
 *
 * Driven from the named scenarios rather than from invented state, so what a test calls
 * "stale" or "degraded" is the same condition the debug drawer and the screenshot suite call
 * by those names.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SyncCentreViewModelTest {
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
    fun `a healthy library reports its last sync, its connection and its outstanding work`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories()
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertFalse(loaded.isLoading)
                assertEquals(ConnectionClass.Unmetered, loaded.connection)
                assertNotNull(loaded.lastCompletedSync)
                assertTrue(loaded.pendingUploads > 0)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an offline library says so rather than reporting a failure`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.Offline)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertTrue(loaded.isOffline)
                assertFalse(loaded.hasFailed)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a fortnight behind with work outstanding raises the staleness warning`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.StaleSync)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertTrue(loaded.isBehindThreshold)
                assertTrue(loaded.showsStaleWarning)
                assertTrue(loaded.behindDays >= STALE_DAYS)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a degraded link is behind but not yet past the threshold`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.SyncDegraded)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertEquals(ConnectionClass.Adverse, loaded.connection)
                assertFalse(loaded.showsStaleWarning)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `snoozing suppresses the warning without touching auto-sync`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.StaleSync)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                assertTrue(expectMostRecentItem().showsStaleWarning)

                viewModel.snoozeStaleWarning(Duration.ofDays(7))
                advanceUntilIdle()
                val snoozed = expectMostRecentItem()

                assertFalse(snoozed.showsStaleWarning)
                assertNotNull(snoozed.snoozedUntil)
                assertTrue(repositories.backend.read { it.settings.autoSyncEnabled })
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `disabling the warning is distinct from snoozing it and is reversible`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.StaleSync)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.snoozeStaleWarning(null)
                advanceUntilIdle()
                val disabled = expectMostRecentItem()
                assertTrue(disabled.isStaleWarningDisabled)
                assertEquals(null, disabled.snoozedUntil)

                viewModel.enableStaleWarning()
                advanceUntilIdle()
                val restored = expectMostRecentItem()

                assertEquals(StaleSyncNotification.Enabled, restored.staleNotification)
                assertTrue(restored.showsStaleWarning)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a forced sync that is refused surfaces its code over content that is still true`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.StaleRevival)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.forceSyncNow()
                advanceUntilIdle()
                val refused = expectMostRecentItem()

                assertEquals("error.upload.stale_revival", refused.failure?.value)
                assertFalse(refused.hasFailed)
                assertFalse(refused.isSyncing)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a forced sync that succeeds advances the last completed timestamp`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.StaleSync)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val before = expectMostRecentItem().lastCompletedSync
                viewModel.forceSyncNow()
                advanceUntilIdle()
                val after = expectMostRecentItem().lastCompletedSync

                assertNotNull(after)
                assertTrue(requireNotNull(after).isAfter(requireNotNull(before)))
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `changing the fetch scope is written through`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories()
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.setScope(SyncScope.MetadataOnly)
                advanceUntilIdle()

                assertEquals(SyncScope.MetadataOnly, expectMostRecentItem().scope)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `held items are counted so the surface can be found from here`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories()
            val held =
                QuarantineItem(
                    assetId = null,
                    surface = QuarantineSurface.RestoreChainConflict,
                    detectedAt = Instant.EPOCH,
                    action = QuarantineAction.ChooseHistory,
                )
            val viewModel = viewModel(repositories, SyncFixture.quarantine(held))

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()

                assertEquals(1, expectMostRecentItem().quarantined)
                cancelAndIgnoreRemainingEvents()
            }
        }

    private fun viewModel(
        repositories: MockRepositories,
        quarantine: QuarantineSource = SyncFixture.quarantine(),
    ) = SyncCentreViewModel(
        sync = repositories.sync,
        settings = repositories.settings,
        timeline = repositories.timeline,
        peers = PeerDiscovery { flowOf(emptyList()) },
        quarantine = quarantine,
        clock = SyncFixture.CLOCK,
    )

    private companion object {
        /** The threshold the design names, in days. */
        const val STALE_DAYS = 14L
    }
}
