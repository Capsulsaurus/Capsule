package com.justin13888.capsule.feature.recovery

import app.cash.turbine.test
import com.justin13888.capsule.core.data.EscrowError
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.RecoveryCadence
import com.justin13888.capsule.core.model.RecoveryState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test
import java.time.Duration

/**
 * The hub's state machine, and the cadence it reports.
 *
 * The cases here are the schedule the design fixes — 7 days, then 90, then a 180-day ceiling
 * — plus the two things a snooze must never become: a lock, or a way to make the prompt
 * disappear for good.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class RecoveryStatusViewModelTest {
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
    fun `it opens loading and settles on a healthy standing`() =
        runTest(dispatcher) {
            val viewModel = RecoveryStatusViewModel(RecoveryFixtures.repositories().recovery)

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the first state is loading, before anything is read")
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertNull(settled.failure)
                assertInstanceOf(RecoveryState.Healthy::class.java, settled.recovery)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the interval widens with each success, up to the ceiling`() =
        runTest(dispatcher) {
            assertEquals(Duration.ofDays(7), RecoveryCadence.intervalAfter(0))
            assertEquals(Duration.ofDays(90), RecoveryCadence.intervalAfter(1))
            assertEquals(Duration.ofDays(180), RecoveryCadence.intervalAfter(2))
            assertEquals(Duration.ofDays(180), RecoveryCadence.intervalAfter(9))
        }

    @Test
    fun `a prompt that has come due says so and can be deferred`() =
        runTest(dispatcher) {
            val backend = RecoveryFixtures.backend()
            backend.mutate { state -> state.recoveryLastVerifiedAt = backend.now().minus(Duration.ofDays(200)) }
            val viewModel = RecoveryStatusViewModel(MockRepositories.from(backend).recovery)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertInstanceOf(RecoveryState.VerificationDue::class.java, state.recovery)
            assertTrue(state.canSnooze)
        }

    @Test
    fun `snoozing suppresses the ask and nothing else`() =
        runTest(dispatcher) {
            val backend = RecoveryFixtures.backend()
            backend.mutate { state -> state.recoveryLastVerifiedAt = backend.now().minus(Duration.ofDays(200)) }
            val viewModel = RecoveryStatusViewModel(MockRepositories.from(backend).recovery)
            advanceUntilIdle()

            viewModel.snooze(RecoveryStatusViewModel.SNOOZE_WEEK)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertInstanceOf(RecoveryState.Snoozed::class.java, state.recovery)
            assertFalse(state.snoozing, "the write has finished")
            assertNull(state.failure)
        }

    @Test
    fun `past the third snooze the reminder stays put rather than disappearing`() =
        runTest(dispatcher) {
            val backend = RecoveryFixtures.backend()
            backend.mutate { state -> state.recoveryLastVerifiedAt = backend.now().minus(Duration.ofDays(200)) }
            val viewModel = RecoveryStatusViewModel(MockRepositories.from(backend).recovery)
            advanceUntilIdle()

            repeat(RecoveryCadence.MAX_CONSECUTIVE_SNOOZES) {
                viewModel.snooze(RecoveryStatusViewModel.SNOOZE_DAY)
                advanceUntilIdle()
            }

            val state = viewModel.state.value
            assertTrue(state.isPersistentBadge, "the prompt becomes a standing badge")
            assertFalse(state.canSnooze, "and offering a fourth snooze would be offering nothing")
        }

    @Test
    fun `repeated misses ask for a fresh secret rather than locking anything`() =
        runTest(dispatcher) {
            val backend = RecoveryFixtures.backend()
            backend.mutate { state -> state.recoveryFailures = RecoveryCadence.FAILURES_BEFORE_REWRAP }
            val viewModel = RecoveryStatusViewModel(MockRepositories.from(backend).recovery)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.rewrapRecommended)
            assertFalse(state.canSnooze, "there is nothing left to defer — the answer is a new secret")
        }

    @Test
    fun `a refusal surfaces its catalogue code, and retrying recovers`() =
        runTest(dispatcher) {
            val backend = MockBackend(RecoveryFixtures.library(), RecoveryFixtures.clock(), MockScenario.Default)
            backend.applyScenario(
                MockScenario.Default.failing(MockOperation.RecoveryRead, EscrowError.Malformed),
            )
            val viewModel = RecoveryStatusViewModel(MockRepositories.from(backend).recovery)
            advanceUntilIdle()
            // The cadence is derived from local state, so a forced read failure surfaces only
            // where the repository actually gates: on the snooze write below.
            viewModel.snooze(RecoveryStatusViewModel.SNOOZE_DAY)
            advanceUntilIdle()

            assertEquals(
                EscrowError.Malformed.code,
                viewModel.state.value.failure
                    ?.code,
            )

            backend.applyScenario(MockScenario.Default)
            viewModel.reload()
            advanceUntilIdle()

            assertNull(viewModel.state.value.failure)
        }

    @Test
    fun `nothing on this state can be read as permission to block another flow`() =
        runTest(dispatcher) {
            val viewModel = settled()

            // A tripwire rather than a behaviour test: the cadence is advisory, so the day
            // somebody adds a `isBlocked`-shaped field here is the day it stops being advisory.
            val names = RecoveryStatusUiState::class.java.declaredFields.map { it.name }
            assertTrue(
                names.none { it.contains("block", ignoreCase = true) || it.contains("gate", ignoreCase = true) },
                "found a gate-shaped field: $names",
            )
            assertFalse(viewModel.state.value.isLoading)
        }

    private fun TestScope.settled(): RecoveryStatusViewModel =
        RecoveryStatusViewModel(RecoveryFixtures.repositories().recovery).also {
            advanceUntilIdle()
            check(!it.state.value.isLoading) { "the fixture should settle without latency" }
        }
}
