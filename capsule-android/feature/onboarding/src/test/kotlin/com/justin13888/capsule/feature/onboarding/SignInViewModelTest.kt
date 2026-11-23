package com.justin13888.capsule.feature.onboarding

import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** Every auth path, and the refusals each of them has to render. */
class SignInViewModelTest {
    @get:Rule
    val mainDispatcher = MainDispatcherRule()

    private val gateway = FixtureAuthGateway()

    private fun viewModelFor(method: AuthMethod): SignInViewModel {
        val session = OnboardingSession()
        session.chooseServer(
            address = "https://photos.example.org",
            info = FIXTURE_SERVER,
            method = method,
        )
        return SignInViewModel(gateway, session)
    }

    @Test
    fun `the password path raises the time-based challenge before it completes`() =
        runTest {
            val viewModel = viewModelFor(AuthMethod.PasswordTotp)
            viewModel.emailChanged("ada@example.org")
            viewModel.passwordChanged("a-long-enough-password")
            viewModel.submitCredentials()
            advanceUntilIdle()

            assertEquals(SignInStage.Totp, viewModel.state.value.stage)
            assertFalse(viewModel.state.value.isComplete)

            viewModel.totpCodeChanged("123456")
            viewModel.submitTotp()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isComplete)
        }

    @Test
    fun `a refused credential carries the catalogue code and clears the in-flight flag`() =
        runTest {
            val viewModel = viewModelFor(AuthMethod.PasswordTotp)
            viewModel.emailChanged("ada@example.org")
            viewModel.passwordChanged("short")
            viewModel.submitCredentials()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertFalse(state.isWorking)
            assertEquals("error.auth.invalid_credentials", state.failure?.code?.value)
        }

    @Test
    fun `repeated refusals escalate to the rate limit the real server would report`() =
        runTest {
            val viewModel = viewModelFor(AuthMethod.PasswordTotp)
            viewModel.emailChanged("ada@example.org")
            repeat(4) {
                viewModel.passwordChanged("short")
                viewModel.submitCredentials()
                advanceUntilIdle()
            }

            assertEquals(
                "error.auth.rate_limited",
                viewModel.state.value.failure
                    ?.code
                    ?.value,
            )
        }

    @Test
    fun `a wrong one-time code is refused without losing the challenge`() =
        runTest {
            val viewModel = viewModelFor(AuthMethod.PasswordTotp)
            viewModel.emailChanged("ada@example.org")
            viewModel.passwordChanged("a-long-enough-password")
            viewModel.submitCredentials()
            advanceUntilIdle()
            viewModel.totpCodeChanged("000000")
            viewModel.submitTotp()
            advanceUntilIdle()

            assertEquals(SignInStage.Totp, viewModel.state.value.stage)
            assertEquals(
                "error.auth.invalid_credentials",
                viewModel.state.value.failure
                    ?.code
                    ?.value,
            )
        }

    @Test
    fun `registering an address that is taken says so`() =
        runTest {
            val viewModel = viewModelFor(AuthMethod.PasswordTotp)
            viewModel.toggleRegistering()
            viewModel.emailChanged("taken@example.org")
            viewModel.passwordChanged("a-long-enough-password")
            viewModel.submitCredentials()
            advanceUntilIdle()

            assertEquals(
                "error.auth.user_already_exists",
                viewModel.state.value.failure
                    ?.code
                    ?.value,
            )
        }

    @Test
    fun `the passkey path opens on its own stage and needs nothing typed`() =
        runTest {
            val viewModel = viewModelFor(AuthMethod.Passkey)

            assertEquals(SignInStage.Passkey, viewModel.state.value.stage)

            viewModel.authenticateWithPasskey()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isComplete)
        }

    @Test
    fun `the identity-provider path opens on its own stage`() =
        runTest {
            val viewModel = viewModelFor(AuthMethod.Oidc)

            assertEquals(SignInStage.Oidc, viewModel.state.value.stage)

            viewModel.completeOidc()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isComplete)
        }
}
