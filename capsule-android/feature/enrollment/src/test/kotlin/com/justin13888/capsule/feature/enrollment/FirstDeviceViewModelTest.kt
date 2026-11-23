package com.justin13888.capsule.feature.enrollment

import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import kotlin.random.Random

/**
 * First-device setup against the seeded backend.
 *
 * Every failure state here is reached by choosing a scenario rather than by substituting a
 * hand-written double, which is what keeps "this is the state the app is in" and "this is the
 * state the test checks" the same sentence.
 */
class FirstDeviceViewModelTest {
    @get:Rule
    val mainDispatcher = MainDispatcherRule()

    private fun viewModelFor(scenario: MockScenario): FirstDeviceViewModel {
        val repositories = MockRepositories.create(scenario = scenario)
        val secureElement = MockSecureElement(repositories.backend)
        return FirstDeviceViewModel(
            ceremony = MockFirstDeviceCeremony(repositories, secureElement),
            random = Random(1),
        )
    }

    private fun FirstDeviceUiState.statusOf(step: EnrollmentStep) = steps.first { it.step == step }.status

    @Test
    fun `every step succeeds and the ceremony stops at the passphrase`() =
        runTest {
            val viewModel = viewModelFor(MockScenario.Default)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.stepsSettled)
            assertEquals(SetupPhase.Passphrase, state.phase)
            assertNotNull(state.passphrase)
        }

    @Test
    fun `a secure element that refuses stops the ceremony where it stands`() =
        runTest {
            val scenario =
                MockScenario.Default.failing(
                    MockOperation.Enrollment,
                    LocalError.Storage("secure element refused"),
                )
            val viewModel = viewModelFor(scenario)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(StepStatus.Done, state.statusOf(EnrollmentStep.MasterKey))
            assertEquals(StepStatus.Failed, state.statusOf(EnrollmentStep.DeviceKeys))
            assertTrue(state.isBlocked)
            assertEquals(SetupPhase.Steps, state.phase)
            assertNull(state.passphrase)
            assertEquals(EnrollmentFailureKind.SecureElementRefused, state.failure?.kind)
        }

    @Test
    fun `an unreachable server defers the directory and finishes setup anyway`() =
        runTest {
            val viewModel = viewModelFor(MockScenario.Offline)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(StepStatus.Deferred, state.statusOf(EnrollmentStep.DeviceDirectory))
            assertEquals(StepStatus.Done, state.statusOf(EnrollmentStep.DefaultAlbum))
            assertFalse(state.isBlocked)
            assertEquals(SetupPhase.Passphrase, state.phase)
        }

    @Test
    fun `a default album that cannot be created does not block setup either`() =
        runTest {
            val viewModel = viewModelFor(MockScenario.QuotaGraceExpired)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(StepStatus.Deferred, state.statusOf(EnrollmentStep.DefaultAlbum))
            assertEquals("error.quota.grace_locked", state.failure?.code?.value)
            assertEquals(SetupPhase.Passphrase, state.phase)
        }

    @Test
    fun `retrying a blocked step re-runs only what has not already succeeded`() =
        runTest {
            val repositories =
                MockRepositories.create(
                    scenario =
                        MockScenario.Default.failing(
                            MockOperation.Enrollment,
                            LocalError.Storage("secure element refused"),
                        ),
                )
            val viewModel =
                FirstDeviceViewModel(
                    ceremony = MockFirstDeviceCeremony(repositories, MockSecureElement(repositories.backend)),
                    random = Random(1),
                )
            advanceUntilIdle()
            assertTrue(viewModel.state.value.isBlocked)

            repositories.applyScenario(MockScenario.Default)
            viewModel.retry(EnrollmentStep.DeviceKeys)
            advanceUntilIdle()

            assertEquals(SetupPhase.Passphrase, viewModel.state.value.phase)
        }

    @Test
    fun `setup does not complete until the words come back correctly`() =
        runTest {
            val viewModel = viewModelFor(MockScenario.Default)
            advanceUntilIdle()
            val passphrase = requireNotNull(viewModel.state.value.passphrase)

            viewModel.continueToTypeBack()
            val challenge = requireNotNull(viewModel.state.value.challenge)
            challenge.positions.forEach { viewModel.answerChanged(it, "wrong") }
            viewModel.submitTypeBack()

            assertTrue(viewModel.state.value.typeBackRejected)
            assertEquals(SetupPhase.TypeBack, viewModel.state.value.phase)

            challenge.positions.forEach { viewModel.answerChanged(it, passphrase.words[it]) }
            viewModel.submitTypeBack()

            assertEquals(SetupPhase.Complete, viewModel.state.value.phase)
        }

    @Test
    fun `the passphrase leaves memory the moment the gate is passed`() =
        runTest {
            val viewModel = viewModelFor(MockScenario.Default)
            advanceUntilIdle()
            val passphrase = requireNotNull(viewModel.state.value.passphrase)
            viewModel.continueToTypeBack()
            val challenge = requireNotNull(viewModel.state.value.challenge)
            challenge.positions.forEach { viewModel.answerChanged(it, passphrase.words[it]) }
            viewModel.submitTypeBack()

            val state = viewModel.state.value
            assertNull(state.passphrase)
            assertNull(state.challenge)
            assertTrue(state.answers.isEmpty())
        }

    @Test
    fun `going back to the words is the only way off the gate, and it is not a way past it`() =
        runTest {
            val viewModel = viewModelFor(MockScenario.Default)
            advanceUntilIdle()
            viewModel.continueToTypeBack()
            viewModel.backToPassphrase()

            assertEquals(SetupPhase.Passphrase, viewModel.state.value.phase)
            assertNotNull(viewModel.state.value.passphrase)
        }
}
