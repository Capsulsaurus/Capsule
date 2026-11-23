package com.justin13888.capsule.feature.moderation

import app.cash.turbine.test
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockScenario
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
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
 * The report form's state machine.
 *
 * The cases that matter here are the ones about what *leaves* the device: a report has to
 * name a hash, the block that can ride along has to be independent of whether the report
 * lands, and a refusal has to arrive as a catalogue code the screen can localize.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ReportViewModelTest {
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
    fun `a hash-shaped subject opens as a content report with the hash already named`() =
        runTest(dispatcher) {
            val viewModel = ReportViewModel(reports(), ModerationFixtures.CONTENT_HASH)

            val state = viewModel.state.value
            assertEquals(ReportSubjectKind.Content, state.kind)
            assertEquals(ModerationFixtures.CONTENT_HASH, state.contentHash)
            assertFalse(state.canBlock, "a content hash names no account to block")
        }

    @Test
    fun `an account-shaped subject opens as an account report with nothing to send yet`() =
        runTest(dispatcher) {
            val viewModel = ReportViewModel(reports(), ACCOUNT)

            val state = viewModel.state.value
            assertEquals(ReportSubjectKind.Account, state.kind)
            assertEquals("", state.contentHash, "an account is not something an operator can act on by itself")
            assertTrue(state.canBlock)
            assertFalse(state.canSubmit, "without a hash there is nothing to report")
        }

    @Test
    fun `submitting without a hash says what is missing instead of sending nothing`() =
        runTest(dispatcher) {
            val viewModel = ReportViewModel(reports(), ACCOUNT)

            viewModel.submit()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.hashMissing)
            assertEquals(ReportPhase.Editing, viewModel.state.value.phase, "nothing was attempted")
        }

    @Test
    fun `a complete report reaches the home server and settles on sent`() =
        runTest(dispatcher) {
            val viewModel = ReportViewModel(reports(), ModerationFixtures.CONTENT_HASH)
            viewModel.onReasonChange(REASON)

            viewModel.state.test {
                assertEquals(ReportPhase.Editing, awaitItem().phase)
                viewModel.submit()
                assertEquals(ReportPhase.Sending, awaitItem().phase)
                val sent = awaitItem()
                assertEquals(ReportPhase.Sent, sent.phase)
                assertNull(sent.failure)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the optional block is placed even though it is nothing to do with the report`() =
        runTest(dispatcher) {
            val repositories = ModerationFixtures.repositories()
            val viewModel = ReportViewModel(repositories.moderation, ACCOUNT)
            viewModel.onContentHashChange(ModerationFixtures.CONTENT_HASH)
            viewModel.onAlsoBlockChange(true)

            viewModel.submit()
            advanceUntilIdle()

            repositories.moderation.blocks().test {
                assertTrue(
                    awaitItem().any { it.blockedUserId.value == ACCOUNT },
                    "a personal block is the user's own protection, not the administrators' decision",
                )
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a refusal surfaces its catalogue code and keeps what the user typed`() =
        runTest(dispatcher) {
            val scenario =
                MockScenario.Default.failing(MockOperation.ModerationWrite, ModerationError.ReportRateLimited)
            val viewModel =
                ReportViewModel(
                    ModerationFixtures.repositories(scenario).moderation,
                    ModerationFixtures.CONTENT_HASH,
                )
            viewModel.onReasonChange(REASON)

            viewModel.submit()
            advanceUntilIdle()

            val failed = viewModel.state.value
            assertEquals(ReportPhase.Failed, failed.phase)
            assertEquals(ModerationError.ReportRateLimited.code, failed.failure?.code)
            assertEquals(REASON, failed.reason, "a refusal must not cost the user their words")

            viewModel.retry()
            assertEquals(ReportPhase.Editing, viewModel.state.value.phase)
            assertNull(viewModel.state.value.failure)
        }

    /** The one repository this form talks to, over fresh mutable state. */
    private fun reports() = ModerationFixtures.repositories().moderation

    private companion object {
        const val ACCOUNT = "alice@other.tld"
        const val REASON = "This is a photo of my front door."
    }
}
