package com.justin13888.capsule.feature.security

import app.cash.turbine.test
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
import kotlin.random.Random

/**
 * The passkey list and the authenticator factor.
 *
 * The enrollment tests are written around the two-step shape on purpose: a secret handed over
 * and never proved is a real state, and a client that collapsed it into one step would leave a
 * user behind a factor they never verified.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class CredentialsViewModelTest {
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
    fun `it opens loading and settles on an account with nothing registered`() =
        runTest(dispatcher) {
            val viewModel = CredentialsViewModel(repository())

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the first state is loading, before anything is read")
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertTrue(settled.hasNoPasskeys)
                assertFalse(settled.isTotpActive)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `adding a passkey lists it`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.addPasskey()
            advanceUntilIdle()

            assertEquals(1, viewModel.state.value.passkeys.size)
            assertTrue(
                viewModel.state.value.passkeys
                    .first()
                    .label
                    .isNotBlank(),
                "a row needs a name",
            )
        }

    @Test
    fun `a newly registered passkey has never been used, and says so`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.addPasskey()
            advanceUntilIdle()

            assertNull(
                viewModel.state.value.passkeys
                    .first()
                    .lastUsedAt,
                "never used is a state a user is looking for, not one to hide behind a creation date",
            )
        }

    @Test
    fun `deleting a passkey removes it`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.addPasskey()
            advanceUntilIdle()
            val target =
                viewModel.state.value.passkeys
                    .first()
                    .id

            viewModel.deletePasskey(target)
            advanceUntilIdle()

            assertTrue(viewModel.state.value.hasNoPasskeys)
        }

    @Test
    fun `the last passkey with no authenticator is flagged and still removable`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.addPasskey()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isLastFactor, "the warning is owed")

            viewModel.deletePasskey(
                viewModel.state.value.passkeys
                    .first()
                    .id,
            )
            advanceUntilIdle()
            assertTrue(
                viewModel.state.value.hasNoPasskeys,
                "the account key still opens the account, so the choice is the user's",
            )
        }

    @Test
    fun `an enrollment hands over a secret before it is active`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.beginTotpEnrollment()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.isEnrolling, "a secret is out but nothing has been proved yet")
            assertFalse(state.isTotpActive)
            assertNotNull(state.totp)
        }

    @Test
    fun `a matching code activates the factor and clears the field`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.beginTotpEnrollment()
            advanceUntilIdle()

            viewModel.onCodeChange(GOOD_CODE)
            viewModel.confirmTotp()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isTotpActive)
            assertEquals("", viewModel.state.value.code, "a spent code is not left on screen")
        }

    @Test
    fun `a rejected code keeps what was typed so one digit can be fixed`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.beginTotpEnrollment()
            advanceUntilIdle()

            viewModel.onCodeChange(SHORT_CODE)
            viewModel.confirmTotp()
            advanceUntilIdle()

            assertEquals(SecurityFailure.TotpCode, viewModel.state.value.failure)
            assertEquals(SHORT_CODE, viewModel.state.value.code)
            assertFalse(viewModel.state.value.isTotpActive)
        }

    @Test
    fun `the code field takes digits only and stops at six`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.onCodeChange("12a34b5678")

            assertEquals("123456", viewModel.state.value.code)
        }

    @Test
    fun `cancelling an unconfirmed enrollment discards its secret`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.beginTotpEnrollment()
            advanceUntilIdle()

            viewModel.cancelTotpEnrollment()
            advanceUntilIdle()

            assertNull(viewModel.state.value.totp, "an abandoned secret is not left half-enrolled")
        }

    @Test
    fun `turning the factor off costs a code exactly as turning it on did`() =
        runTest(dispatcher) {
            val repository = repository()
            val viewModel = CredentialsViewModel(repository)
            advanceUntilIdle()
            viewModel.beginTotpEnrollment()
            advanceUntilIdle()
            viewModel.onCodeChange(GOOD_CODE)
            viewModel.confirmTotp()
            advanceUntilIdle()

            viewModel.onCodeChange(SHORT_CODE)
            viewModel.disableTotp()
            advanceUntilIdle()
            assertTrue(viewModel.state.value.isTotpActive, "a bad code does not drop a second factor")

            viewModel.onCodeChange(GOOD_CODE)
            viewModel.disableTotp()
            advanceUntilIdle()

            assertNull(repository.totp().first())
            assertFalse(viewModel.state.value.isTotpActive)
        }

    private fun repository(): CredentialRepository =
        InMemoryCredentialRepository(
            clock = SecurityFixtures.clock(),
            random = Random(SEED),
            deviceLabel = DEVICE_LABEL,
        )

    private fun TestScope.settled(): CredentialsViewModel {
        val viewModel = CredentialsViewModel(repository())
        advanceUntilIdle()
        return viewModel
    }

    private companion object {
        const val GOOD_CODE = "123456"
        const val SHORT_CODE = "12345"
        const val DEVICE_LABEL = "Pixel 8 Pro"
        const val SEED = 42
    }
}
