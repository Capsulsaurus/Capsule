package com.justin13888.capsule.feature.enrollment

import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * The cross-device ceremony, end to end and failure by failure.
 *
 * The safety-code cases are the ones worth the most: they are the only defence the channel
 * has, and every one of them is reached through the seeded backend's own scenarios rather than
 * through a stubbed gateway that could agree with the test by construction.
 */
class CrossDeviceViewModelTest {
    @get:Rule
    val mainDispatcher = MainDispatcherRule()

    private val session = EnrollmentSession()

    private fun repositoriesFor(scenario: MockScenario) = MockRepositories.create(scenario = scenario)

    @Test
    fun `issuing a code needs fresh proof of physical control first`() =
        runTest {
            val viewModel = EnrollmentStartViewModel(AlwaysConfirmingAuthorization(), session)

            assertFalse(viewModel.state.value.isAuthorized)

            viewModel.confirm()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isAuthorized)
            assertTrue(session.progress.value.isAuthorized)
        }

    @Test
    fun `a declined prompt is said out loud rather than silently ignored`() =
        runTest {
            val declining =
                object : EnrollmentAuthorization {
                    override suspend fun confirm(): Boolean = false
                }
            val viewModel = EnrollmentStartViewModel(declining, session)
            viewModel.confirm()
            advanceUntilIdle()

            assertFalse(viewModel.state.value.isAuthorized)
            assertTrue(viewModel.state.value.wasDeclined)
        }

    @Test
    fun `a code is issued in both forms, and the short one really is shorter`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = EnrollmentCodeViewModel(MockEnrollmentCodeIssuer(repositories.backend), session)
            // `advanceUntilIdle` would run the ten-minute countdown to zero: the view model is
            // never idle while a code is live, which is the point of it.
            runCurrent()

            val code = requireNotNull(viewModel.state.value.code)
            assertTrue(code.friendlyDigits.length < code.payload.length)
            assertTrue(code.friendlyDigits.all(Char::isDigit))
            assertEquals(code, session.progress.value.issuedCode)
        }

    @Test
    fun `the code expires after ten minutes and stops being offered`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = EnrollmentCodeViewModel(MockEnrollmentCodeIssuer(repositories.backend), session)
            // `advanceUntilIdle` would run the ten-minute countdown to zero: the view model is
            // never idle while a code is live, which is the point of it.
            runCurrent()

            assertFalse(viewModel.state.value.isExpired)

            advanceTimeBy(EnrollmentCodeUiState.LIFETIME.toMillis() + 1_000)

            assertTrue(viewModel.state.value.isExpired)
        }

    @Test
    fun `regenerating puts a fresh window back on the clock`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = EnrollmentCodeViewModel(MockEnrollmentCodeIssuer(repositories.backend), session)
            // `advanceUntilIdle` would run the ten-minute countdown to zero: the view model is
            // never idle while a code is live, which is the point of it.
            runCurrent()
            advanceTimeBy(EnrollmentCodeUiState.LIFETIME.toMillis() + 1_000)
            assertTrue(viewModel.state.value.isExpired)

            viewModel.issue()
            runCurrent()

            assertFalse(viewModel.state.value.isExpired)
        }

    @Test
    fun `a code cannot be issued with no path to the server`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Offline)
            val viewModel = EnrollmentCodeViewModel(MockEnrollmentCodeIssuer(repositories.backend), session)
            // `advanceUntilIdle` would run the ten-minute countdown to zero: the view model is
            // never idle while a code is live, which is the point of it.
            runCurrent()

            assertNull(viewModel.state.value.code)
            assertEquals(
                EnrollmentFailureKind.ServerUnreachable,
                viewModel.state.value.failure
                    ?.kind,
            )
        }

    @Test
    fun `the scanner reads nothing until the camera is allowed`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = EnrollmentScanViewModel(MockEnrollmentCodeReader(repositories.backend), session)
            advanceUntilIdle()

            assertFalse(viewModel.state.value.isReading)
            assertNull(viewModel.state.value.acceptedCode)

            viewModel.cameraPermissionChanged(granted = true)
            advanceUntilIdle()

            assertNotNull(viewModel.state.value.acceptedCode)
        }

    @Test
    fun `a hand-typed code goes down the same path a scanned one does`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = EnrollmentScanViewModel(MockEnrollmentCodeReader(repositories.backend), session)
            viewModel.manualCodeChanged("482913057")
            viewModel.submitManualCode()
            advanceUntilIdle()

            assertEquals("482913057", viewModel.state.value.acceptedCode)
            assertEquals("482913057", session.progress.value.redeemedCode)
        }

    @Test
    fun `confirming without acknowledging the match does nothing and says why`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = SafetyCodeViewModel(repositories.enrollment, session)
            viewModel.confirm()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.acknowledgementMissing)
            assertNull(state.outcome)
            assertFalse(state.isConfirming)
        }

    @Test
    fun `an acknowledged match adds the device to the directory`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = SafetyCodeViewModel(repositories.enrollment, session)
            viewModel.acknowledgementChanged(acknowledged = true)
            viewModel.confirm()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.outcome is EnrollmentOutcome.Added)
            assertTrue(session.progress.value.outcome is EnrollmentOutcome.Added)
        }

    @Test
    fun `both devices show the same code, beside each device's own identity`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = SafetyCodeViewModel(repositories.enrollment, session)

            val state = viewModel.state.value
            assertEquals(repositories.enrollment.safetyCode(), state.safetyCode?.text)
            assertNotNull(state.thisDevice?.keyFingerprint)
            assertNotNull(state.otherDevice?.keyFingerprint)
            assertTrue(state.otherDevice?.model?.isNotBlank() == true)
        }

    @Test
    fun `a machine in the middle ends the ceremony rather than warning about it`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.EnrollmentMitm)
            val viewModel = SafetyCodeViewModel(repositories.enrollment, session)
            viewModel.acknowledgementChanged(acknowledged = true)
            viewModel.confirm()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.isAborted)
            assertNull(state.outcome)
            assertEquals("error.enrollment.code_refused", state.failure?.code?.value)
        }

    @Test
    fun `the user saying the codes differ is its own exit and hands nothing over`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Default)
            val viewModel = SafetyCodeViewModel(repositories.enrollment, session)
            viewModel.abort()

            assertTrue(viewModel.state.value.isAborted)
            assertNull(session.progress.value.outcome)

            viewModel.startOver()

            assertFalse(viewModel.state.value.isAborted)
            assertFalse(viewModel.state.value.isAcknowledged)
        }

    @Test
    fun `a server that cannot be reached completes with the upload still pending`() =
        runTest {
            val repositories = repositoriesFor(MockScenario.Offline)
            val viewModel = SafetyCodeViewModel(repositories.enrollment, session)
            viewModel.acknowledgementChanged(acknowledged = true)
            viewModel.confirm()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertFalse(state.isAborted)
            assertEquals(EnrollmentOutcome.PendingDirectoryUpload, state.outcome)
        }

    @Test
    fun `the completion screen only reports what the ceremony decided`() =
        runTest {
            session.finished(EnrollmentOutcome.Added("Pixel 9 Pro"))
            val viewModel = EnrollmentCompleteViewModel(session)
            advanceUntilIdle()

            assertEquals(EnrollmentOutcome.Added("Pixel 9 Pro"), viewModel.state.value.outcome)
        }
}
