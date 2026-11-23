package com.justin13888.capsule.feature.onboarding

import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * The media primer, whose interesting behaviour is entirely about Android 14: the prompt's
 * own result is not the answer, and partial access is not a denial.
 */
class OnboardingCompleteViewModelTest {
    @get:Rule
    val mainDispatcher = MainDispatcherRule()

    private val probe = StubMediaAccessProbe()

    private fun viewModelFor(mode: OnboardingMode): OnboardingCompleteViewModel {
        val session = OnboardingSession()
        session.chooseMode(mode)
        return OnboardingCompleteViewModel(probe, session)
    }

    @Test
    fun `the primer shows until the prompt has been raised at least once`() {
        val viewModel = viewModelFor(OnboardingMode.LocalOnly)

        assertTrue(viewModel.state.value.showingPrimer)
    }

    @Test
    fun `picking photos rather than granting everything reads back as partial access`() =
        runTest {
            val viewModel = viewModelFor(OnboardingMode.LocalOnly)
            probe.access = MediaAccess.Partial
            viewModel.promptClosed()
            advanceUntilIdle()

            assertEquals(MediaAccess.Partial, viewModel.state.value.access)
            assertFalse(viewModel.state.value.showingPrimer)
        }

    @Test
    fun `a refusal stops being the primer and becomes an outcome with a way back`() =
        runTest {
            val viewModel = viewModelFor(OnboardingMode.LocalOnly)
            viewModel.promptClosed()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(MediaAccess.None, state.access)
            assertTrue(state.hasAsked)
            assertFalse(state.showingPrimer)
        }

    @Test
    fun `the completion copy is chosen from the mode rather than assumed`() {
        assertEquals(OnboardingMode.LocalOnly, viewModelFor(OnboardingMode.LocalOnly).state.value.mode)
        assertEquals(OnboardingMode.ServerBacked, viewModelFor(OnboardingMode.ServerBacked).state.value.mode)
    }
}
