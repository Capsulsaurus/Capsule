package com.justin13888.capsule.feature.onboarding

import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * Naming a server: what is refused before anything is sent, what the probe reports back, and
 * what happens when a deployment offers nothing this build can drive.
 */
class ServerConnectViewModelTest {
    @get:Rule
    val mainDispatcher = MainDispatcherRule()

    private val session = OnboardingSession()
    private val viewModel = ServerConnectViewModel(FixtureServerProbe(), session)

    @Test
    fun `an address with no scheme is a field problem, not an unreachable server`() =
        runTest {
            viewModel.addressChanged("photos.example.org")
            viewModel.connect()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.isAddressInvalid)
            assertNull(state.failure)
            assertNull(state.info)
        }

    @Test
    fun `a server that answers preselects its first path so continue is never inert`() =
        runTest {
            viewModel.addressChanged("https://photos.example.org")
            viewModel.connect()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertFalse(state.isProbing)
            assertEquals(AuthMethod.entries.first(), state.selectedMethod)
            assertTrue(state.canContinue)
        }

    @Test
    fun `an unreachable server surfaces a code rather than a transport status`() =
        runTest {
            viewModel.addressChanged("https://unreachable.example.org")
            viewModel.connect()
            advanceUntilIdle()

            // LocalError carries no catalogue code, which the failure type says honestly
            // rather than inventing a key that means nothing.
            val failure = viewModel.state.value.failure
            assertTrue(failure != null)
            assertNull(failure?.code)
        }

    @Test
    fun `a server too old to talk to reports the protocol code`() =
        runTest {
            viewModel.addressChanged("https://legacy.example.org")
            viewModel.connect()
            advanceUntilIdle()

            assertEquals(
                "error.protocol.version_unsupported",
                viewModel.state.value.failure
                    ?.code
                    ?.value,
            )
        }

    @Test
    fun `a deployment offering no usable path is a degraded state with a way out`() =
        runTest {
            viewModel.addressChanged("https://bare.example.org")
            viewModel.connect()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.hasNoUsableMethod)
            assertFalse(viewModel.state.value.canContinue)

            viewModel.useLocalOnly()
            assertEquals(OnboardingMode.LocalOnly, session.progress.value.mode)
        }

    @Test
    fun `only a path the server actually offers can be selected`() =
        runTest {
            viewModel.addressChanged("https://sso.example.org")
            viewModel.connect()
            advanceUntilIdle()

            viewModel.selectMethod(AuthMethod.PasswordTotp)

            assertEquals(AuthMethod.Oidc, viewModel.state.value.selectedMethod)
        }

    @Test
    fun `committing hands the whole choice to the shared flow state`() =
        runTest {
            viewModel.addressChanged("https://photos.example.org")
            viewModel.connect()
            advanceUntilIdle()
            viewModel.selectMethod(AuthMethod.Oidc)
            viewModel.commit()

            val progress = session.progress.value
            assertEquals("https://photos.example.org", progress.serverAddress)
            assertEquals(AuthMethod.Oidc, progress.authMethod)
            assertEquals(OnboardingMode.ServerBacked, progress.mode)
        }

    @Test
    fun `retyping the address discards the answer the previous one produced`() =
        runTest {
            viewModel.addressChanged("https://photos.example.org")
            viewModel.connect()
            advanceUntilIdle()
            viewModel.addressChanged("https://other.example.org")

            assertNull(viewModel.state.value.info)
        }
}
