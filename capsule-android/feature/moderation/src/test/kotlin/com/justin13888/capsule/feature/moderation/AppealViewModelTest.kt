package com.justin13888.capsule.feature.moderation

import app.cash.turbine.test
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
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

/**
 * The appeal's state machine, and the record it is filed against.
 *
 * The load-bearing case is the proof: an appeal is authenticated by the account master key
 * rather than by the session, because the session is frequently the thing in dispute. A
 * signature that does not correspond to the challenge the server issued must therefore leave
 * the appeal *unsent* and say so, rather than failing in a way the user reads as "try again".
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AppealViewModelTest {
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
    fun `it opens loading and settles on the account's record`() =
        runTest(dispatcher) {
            val repositories = ModerationFixtures.repositories()
            val viewModel = AppealViewModel(repositories.moderation, gateway, gateway, seededCase)

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the first state is loading, before anything is read")
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertNull(settled.failure)
                assertTrue(settled.events.isNotEmpty(), "the seeded account carries a takedown and its lift")
                assertNotNull(settled.case, "the destination named an entry that is on the record")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a suspended account is told what it can still do`() =
        runTest(dispatcher) {
            val repositories = ModerationFixtures.repositories(MockScenario.AccountSuspended)
            val viewModel = AppealViewModel(repositories.moderation, gateway, gateway, seededCase)
            advanceUntilIdle()

            val effects = viewModel.state.value.effects
            assertNotNull(effects)
            assertTrue(viewModel.state.value.isSuspended)
            assertTrue(
                effects!!.canRevokeAllSessions,
                "revoking every session is gated by the master key, not by account standing",
            )
            assertTrue(effects.canRead, "suspension is an access-level action, never a data-level one")
        }

    @Test
    fun `a case id that is not on the record offers no form to fill in`() =
        runTest(dispatcher) {
            val viewModel = settled(caseId = "not-on-this-record")

            assertTrue(viewModel.state.value.caseMissing)
            assertFalse(viewModel.state.value.canSubmit)
        }

    @Test
    fun `an empty statement says so rather than spending the appeal`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.submit()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.statementMissing)
            assertEquals(AppealPhase.Editing, viewModel.state.value.phase, "nothing was attempted")
        }

    @Test
    fun `a signed appeal walks proving to sending to sent and comes back with a reference`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.onStatementChange(STATEMENT)

            viewModel.state.test {
                assertEquals(AppealPhase.Editing, awaitItem().phase)
                viewModel.submit()
                assertEquals(AppealPhase.Proving, awaitItem().phase)
                assertEquals(AppealPhase.Sending, awaitItem().phase)
                val sent = awaitItem()
                assertEquals(AppealPhase.Sent, sent.phase)
                assertNotNull(sent.reference)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a proof that does not answer the challenge leaves the appeal unsent and says so`() =
        runTest(dispatcher) {
            val repositories = ModerationFixtures.repositories()
            val viewModel =
                AppealViewModel(repositories.moderation, gateway, { ByteArray(1) }, seededCase)
            advanceUntilIdle()
            viewModel.onStatementChange(STATEMENT)

            viewModel.submit()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(AppealPhase.Failed, state.phase)
            assertTrue(state.proofRejected, "the user has to be told nothing was sent")
            assertNull(state.failure, "a rejected proof is not a transport failure and must not read as one")
        }

    @Test
    fun `retrying after a refusal keeps what was written`() =
        runTest(dispatcher) {
            val repositories = ModerationFixtures.repositories()
            val viewModel = AppealViewModel(repositories.moderation, gateway, { ByteArray(1) }, seededCase)
            advanceUntilIdle()
            viewModel.onStatementChange(STATEMENT)
            viewModel.submit()
            advanceUntilIdle()

            viewModel.retry()

            assertEquals(AppealPhase.Editing, viewModel.state.value.phase)
            assertFalse(viewModel.state.value.proofRejected)
            assertEquals(STATEMENT, viewModel.state.value.statement)
        }

    private val gateway = LocalAppealGateway()

    /** An entry the seeded account really carries, so the appeal has something to be about. */
    private val seededCase: String =
        ModerationFixtures
            .library()
            .moderationEvents
            .first()
            .id
            .value

    private fun TestScope.settled(caseId: String = seededCase): AppealViewModel =
        AppealViewModel(ModerationFixtures.repositories().moderation, gateway, gateway, caseId).also {
            advanceUntilIdle()
            check(!it.state.value.isLoading) { "the fixture should settle without latency" }
        }

    private companion object {
        const val STATEMENT = "This photo is of my own front door and breaks no rule I can find."
    }
}
