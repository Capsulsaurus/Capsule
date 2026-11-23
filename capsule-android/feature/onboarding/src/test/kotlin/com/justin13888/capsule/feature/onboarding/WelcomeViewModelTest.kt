package com.justin13888.capsule.feature.onboarding

import app.cash.turbine.test
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * The welcome tour, and the one thing about it that is a product decision rather than a
 * carousel: skipping the tour must not skip the mode choice.
 */
class WelcomeViewModelTest {
    @get:Rule
    val mainDispatcher = MainDispatcherRule()

    private val session = OnboardingSession()
    private val viewModel = WelcomeViewModel(session)

    @Test
    fun `advancing walks the panes and then reveals the mode choice`() =
        runTest {
            viewModel.state.test {
                assertEquals(0, awaitItem().page)
                repeat(WELCOME_PANES - 1) { viewModel.advance() }
                repeat(WELCOME_PANES - 1) { awaitItem() }
                assertFalse(viewModel.state.value.choosingMode)

                viewModel.advance()
                assertTrue(awaitItem().choosingMode)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `skipping the tour lands on the mode choice rather than past it`() {
        viewModel.skipTour()

        assertTrue(viewModel.state.value.choosingMode)
        assertEquals(null, session.progress.value.mode)
    }

    @Test
    fun `choosing the local library records a mode rather than an absence of one`() {
        viewModel.choose(OnboardingMode.LocalOnly)

        assertEquals(OnboardingMode.LocalOnly, session.progress.value.mode)
        assertFalse(session.progress.value.signedIn)
    }

    @Test
    fun `choosing a server after a local library clears nothing the server flow needs`() {
        viewModel.choose(OnboardingMode.LocalOnly)
        viewModel.choose(OnboardingMode.ServerBacked)

        assertEquals(OnboardingMode.ServerBacked, session.progress.value.mode)
    }

    @Test
    fun `panes outside the tour are refused rather than clamped`() {
        viewModel.showPane(WELCOME_PANES)

        assertEquals(0, viewModel.state.value.page)
    }
}
