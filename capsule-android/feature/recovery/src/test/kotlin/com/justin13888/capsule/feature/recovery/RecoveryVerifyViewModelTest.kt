package com.justin13888.capsule.feature.recovery

import app.cash.turbine.test
import com.justin13888.capsule.core.data.EscrowError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.RecoveryCadence
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
 * The local check's state machine.
 *
 * The point of every case here is that a wrong answer is cheap. It costs no lock, no server
 * round trip and no state any other flow reads; the only thing it eventually buys is an offer
 * to mint a secret the user does hold.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class RecoveryVerifyViewModelTest {
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
    fun `the right passphrase verifies and names the next prompt`() =
        runTest(dispatcher) {
            val viewModel = RecoveryVerifyViewModel(RecoveryFixtures.repositories().recovery)
            advanceUntilIdle()
            viewModel.onSecretChange(RecoveryFixtures.CORRECT_SECRET)

            viewModel.state.test {
                assertEquals(VerifyPhase.Editing, awaitItem().phase)
                viewModel.verify()
                assertEquals(VerifyPhase.Checking, awaitItem().phase)
                val verified = awaitItem()
                assertEquals(VerifyPhase.Verified, verified.phase)
                assertEquals(0, verified.consecutiveFailures)
                assertEquals("", verified.secret, "a verified phrase is not left sitting in the field")
                cancelAndIgnoreRemainingEvents()
            }
            advanceUntilIdle()
            assertNotNull(viewModel.state.value.nextPromptAt, "the cadence answers when it will ask again")
        }

    @Test
    fun `a wrong passphrase misses, and misses are the only thing that accumulates`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.onSecretChange(RecoveryFixtures.WRONG_SECRET)

            viewModel.verify()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(VerifyPhase.NotVerified, state.phase)
            assertEquals(1, state.consecutiveFailures)
            assertNull(state.failure, "a miss is not a failure of the check")
            assertFalse(state.rewrapRecommended, "one miss is a typo, not a lost secret")
        }

    @Test
    fun `enough misses offer a fresh secret instead of refusing further attempts`() =
        runTest(dispatcher) {
            val viewModel = settled()

            repeat(RecoveryCadence.FAILURES_BEFORE_REWRAP) {
                viewModel.onSecretChange(RecoveryFixtures.WRONG_SECRET)
                viewModel.verify()
                advanceUntilIdle()
            }

            val state = viewModel.state.value
            assertTrue(state.rewrapRecommended)
            assertTrue(state.canSubmit, "the field is still live — nothing has been locked")
        }

    @Test
    fun `a correct passphrase after misses clears the count`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.onSecretChange(RecoveryFixtures.WRONG_SECRET)
            viewModel.verify()
            advanceUntilIdle()

            viewModel.onSecretChange(RecoveryFixtures.CORRECT_SECRET)
            viewModel.verify()
            advanceUntilIdle()

            assertEquals(0, viewModel.state.value.consecutiveFailures)
            assertEquals(VerifyPhase.Verified, viewModel.state.value.phase)
        }

    @Test
    fun `an empty field says what is missing rather than checking nothing`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.verify()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.secretMissing)
            assertEquals(VerifyPhase.Editing, viewModel.state.value.phase)
            assertEquals(0, viewModel.state.value.consecutiveFailures, "nothing was attempted")
        }

    @Test
    fun `the check being unavailable is reported as its own thing, not as a wrong answer`() =
        runTest(dispatcher) {
            val scenario = MockScenario.Default.failing(MockOperation.RecoveryRead, EscrowError.Malformed)
            val viewModel = RecoveryVerifyViewModel(RecoveryFixtures.repositories(scenario).recovery)
            advanceUntilIdle()
            viewModel.onSecretChange(RecoveryFixtures.CORRECT_SECRET)

            viewModel.verify()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(VerifyPhase.Failed, state.phase)
            assertEquals(EscrowError.Malformed.code, state.failure?.code)
            assertEquals(0, state.consecutiveFailures, "an unreadable escrow is not evidence about the passphrase")
        }

    @Test
    fun `revealing the phrase is a plain toggle and does not touch the check`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.onRevealChange(true)

            assertTrue(viewModel.state.value.revealed)
            assertEquals(VerifyPhase.Editing, viewModel.state.value.phase)
        }

    @Test
    fun `resetting clears the field and the outcome`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.onSecretChange(RecoveryFixtures.WRONG_SECRET)
            viewModel.verify()
            advanceUntilIdle()

            viewModel.reset()

            assertEquals(VerifyPhase.Editing, viewModel.state.value.phase)
            assertEquals("", viewModel.state.value.secret)
        }

    private fun TestScope.settled(): RecoveryVerifyViewModel =
        RecoveryVerifyViewModel(RecoveryFixtures.repositories().recovery).also { advanceUntilIdle() }
}
