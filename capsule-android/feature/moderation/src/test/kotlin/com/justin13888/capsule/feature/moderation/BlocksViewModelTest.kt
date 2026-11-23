package com.justin13888.capsule.feature.moderation

import app.cash.turbine.test
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockScenario
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
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

/**
 * The blocked list's state machine.
 *
 * Blocking is the one moderation action a user takes entirely on their own authority, so the
 * cases here are about it staying that way: idempotent, reversible, per row, and never
 * silently widened into anything the server would apply on someone else's behalf.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class BlocksViewModelTest {
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
    fun `it opens loading and settles on an empty list`() =
        runTest(dispatcher) {
            val viewModel = BlocksViewModel(ModerationFixtures.repositories().moderation)

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the first state is loading, before anything is read")
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertTrue(settled.isEmpty, "a fresh library has blocked nobody")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `blocking an account lands on the list and clears the field`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.onDraftChange(ACCOUNT)
            viewModel.block()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(listOf(ACCOUNT), state.blocks.map { it.blockedUserId.value })
            assertEquals("", state.draft, "a placed block should not sit in the field waiting to be placed again")
            assertFalse(state.isPending(ACCOUNT))
        }

    @Test
    fun `blocking the same account twice changes nothing`() =
        runTest(dispatcher) {
            val viewModel = settled()

            repeat(2) {
                viewModel.onDraftChange(ACCOUNT)
                viewModel.block()
                advanceUntilIdle()
            }

            assertEquals(1, viewModel.state.value.blocks.size, "re-blocking is idempotent")
        }

    @Test
    fun `an empty field says what is missing rather than blocking nobody`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.block()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.draftMissing)
            assertTrue(
                viewModel.state.value.blocks
                    .isEmpty(),
            )
        }

    @Test
    fun `unblocking lifts it`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.onDraftChange(ACCOUNT)
            viewModel.block()
            advanceUntilIdle()

            viewModel.unblock(ACCOUNT)
            advanceUntilIdle()

            assertTrue(
                viewModel.state.value.blocks
                    .isEmpty(),
            )
        }

    @Test
    fun `a refused write surfaces its catalogue code and leaves the list readable`() =
        runTest(dispatcher) {
            val scenario = MockScenario.Default.failing(MockOperation.ModerationWrite, ModerationError.ServerBlocked)
            val viewModel = settled(scenario)

            viewModel.onDraftChange(ACCOUNT)
            viewModel.block()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(ModerationError.ServerBlocked.code, state.failure?.code)
            assertFalse(state.isPending(ACCOUNT), "a failed write must not leave a row stuck busy")
            assertNull(state.blocks.firstOrNull())
        }

    private fun TestScope.settled(scenario: MockScenario = MockScenario.Default): BlocksViewModel =
        BlocksViewModel(ModerationFixtures.repositories(scenario).moderation).also {
            advanceUntilIdle()
            check(!it.state.value.isLoading) { "the fixture should settle without latency" }
        }

    private companion object {
        const val ACCOUNT = "alice@other.tld"
    }
}
