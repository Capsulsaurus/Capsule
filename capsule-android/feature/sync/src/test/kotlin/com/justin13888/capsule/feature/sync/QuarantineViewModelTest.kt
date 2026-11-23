package com.justin13888.capsule.feature.sync

import app.cash.turbine.test
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.QuarantineSurface
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import java.time.Instant

/**
 * The quarantine surface's state machine, and the surface-to-action table behind it.
 *
 * The table is the part worth pinning down: every refusal has to reach the user with a reason
 * and one explicit thing to do, and no surface may resolve itself — so a new surface added
 * without a decision about how it is presented shows up here as a failure rather than as a
 * silently applied write.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class QuarantineViewModelTest {
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
    fun `the seeded library's held item is a stale revival, offering to keep this device's copy`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories()
            val viewModel = QuarantineViewModel(TimelineQuarantineSource(repositories.timeline))

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertEquals(1, loaded.items.size)
                assertEquals(QuarantineSurface.StaleRevival, loaded.items.single().surface)
                assertEquals(QuarantineAction.KeepMine, loaded.items.single().action)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `every surface offers exactly one explicit action and none of them applies the change`() {
        val actions = QuarantineSurface.entries.associateWith { it.defaultAction() }

        assertEquals(QuarantineAction.KeepMine, actions[QuarantineSurface.StaleRevival])
        assertEquals(QuarantineAction.ChooseHistory, actions[QuarantineSurface.RestoreChainConflict])
        assertEquals(QuarantineAction.Discard, actions[QuarantineSurface.VerifyAssetReject])
        assertEquals(QuarantineAction.Inspect, actions[QuarantineSurface.MalformedSidecar])
        assertEquals(QuarantineAction.OpenInbox, actions[QuarantineSurface.PendingDropReview])
        assertEquals(QuarantineSurface.entries.size, actions.size)
    }

    @Test
    fun `a surface this build does not recognise is only ever inspectable`() {
        assertEquals(QuarantineAction.Inspect, null.defaultAction())
    }

    @Test
    fun `resolving one item leaves the others alone`() =
        runTest(dispatcher) {
            val tampered = item(QuarantineSurface.MalformedSidecar)
            val conflict = item(QuarantineSurface.RestoreChainConflict)
            val viewModel = QuarantineViewModel(SyncFixture.quarantine(tampered, conflict))

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                assertEquals(2, expectMostRecentItem().items.size)

                viewModel.resolve(tampered)
                val remaining = awaitItem()

                assertEquals(listOf(conflict.key), remaining.items.map(QuarantineItem::key))
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an empty surface says so rather than looking broken`() =
        runTest(dispatcher) {
            val viewModel = QuarantineViewModel(SyncFixture.quarantine())

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()

                assertTrue(expectMostRecentItem().isEmpty)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a source that cannot be read surfaces an error with a way back`() =
        runTest(dispatcher) {
            val viewModel = QuarantineViewModel { throw LocalError.Storage("the index is locked") }

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val failed = expectMostRecentItem()

                assertTrue(failed.hasFailed)
                assertTrue(failed.hasLocalFailure)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an offline library still lists what is held, because quarantine is local`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.Offline)
            val viewModel = QuarantineViewModel(TimelineQuarantineSource(repositories.timeline))

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()

                assertEquals(1, expectMostRecentItem().items.size)
                cancelAndIgnoreRemainingEvents()
            }
        }

    private fun item(surface: QuarantineSurface) =
        QuarantineItem(
            assetId = null,
            surface = surface,
            detectedAt = Instant.parse("2026-06-01T00:00:00Z"),
            action = surface.defaultAction(),
        )
}
