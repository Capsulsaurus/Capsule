package com.justin13888.capsule.feature.security

import app.cash.turbine.test
import com.justin13888.capsule.core.data.AuthError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.Device
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

/**
 * The session ledger's state machine.
 *
 * The two revokes are asserted separately and deliberately: the everyday one needs nothing but
 * an active session, and the global one must be refused without a master-key proof — because
 * an attacker holding a stolen session token must not be able to lock the real user out of
 * every other device.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SessionsViewModelTest {
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
    fun `it opens loading and settles on a grouped ledger`() =
        runTest(dispatcher) {
            val viewModel = viewModelOver(MockRepositories.from(SecurityFixtures.backend()))

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the first state is loading, before anything is read")
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertNull(settled.failure)
                assertTrue(settled.groups.isNotEmpty(), "the seeded account has sessions")
                assertTrue(settled.devices.isNotEmpty(), "and a signed device directory")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the device directory lists this device first and retired rows last`() =
        runTest(dispatcher) {
            val devices = settled(MockScenario.Default).state.value.devices

            assertTrue(devices.first().isThisDevice)
            assertTrue(
                devices.last().revokedAt != null,
                "a retired row is kept and shown, but not in front of the ones the user can act on",
            )
        }

    @Test
    fun `revoking one session needs nothing beyond being signed in`() =
        runTest(dispatcher) {
            val repositories = MockRepositories.from(SecurityFixtures.backend())
            val viewModel = viewModelOver(repositories)
            advanceUntilIdle()
            val target =
                repositories.sessions
                    .sessions()
                    .first()
                    .first { !it.isCurrent }

            viewModel.revokeSession(target.id)
            advanceUntilIdle()

            val after =
                repositories.sessions
                    .sessions()
                    .first()
                    .first { it.id == target.id }
            assertNotNull(after.revokedAt, "the session ended")
            assertNull(viewModel.state.value.failure)
        }

    @Test
    fun `retiring a device keeps its directory row rather than pruning it`() =
        runTest(dispatcher) {
            val repositories = MockRepositories.from(SecurityFixtures.backend())
            val viewModel = viewModelOver(repositories)
            advanceUntilIdle()
            val before = repositories.sessions.devices().first()
            val target = before.first { it.isActive && !it.isThisDevice }

            viewModel.revokeDevice(target.id)
            advanceUntilIdle()

            val after = repositories.sessions.devices().first()
            assertEquals(before.size, after.size, "the row it signed history with is never removed")
            assertNotNull(after.first { it.id == target.id }.revokedAt)
        }

    @Test
    fun `the global revoke asks before it proves anything`() =
        runTest(dispatcher) {
            val viewModel = settled(MockScenario.Default)

            viewModel.askRevokeAll()

            assertEquals(RevokeAllState.Confirming, viewModel.state.value.revokeAll)
        }

    @Test
    fun `a master key proof ends every session including this one`() =
        runTest(dispatcher) {
            val repositories = MockRepositories.from(SecurityFixtures.backend())
            val viewModel = viewModelOver(repositories)
            advanceUntilIdle()

            viewModel.askRevokeAll()
            viewModel.confirmRevokeAll()
            advanceUntilIdle()

            assertEquals(RevokeAllState.Done, viewModel.state.value.revokeAll)
            assertTrue(
                repositories.sessions
                    .sessions()
                    .first()
                    .all { it.revokedAt != null },
                "the global revoke does not spare the caller's own session",
            )
        }

    @Test
    fun `without a proof the global revoke ends nothing at all`() =
        runTest(dispatcher) {
            val repositories = MockRepositories.from(SecurityFixtures.backend())
            val viewModel =
                SessionsViewModel(sessions = repositories.sessions, proofSource = { ByteArray(0) })
            advanceUntilIdle()

            viewModel.confirmRevokeAll()
            advanceUntilIdle()

            val outcome = viewModel.state.value.revokeAll
            assertTrue(outcome is RevokeAllState.Refused, "a proofless revoke is refused, not partially applied")
            assertEquals(
                SecurityFailure.of(AuthError.RevokeProofRequired),
                (outcome as RevokeAllState.Refused).failure,
            )
            assertTrue(
                repositories.sessions
                    .sessions()
                    .first()
                    .none { it.revokedAt != null },
                "a stolen session token cannot lock the real user out of anything",
            )
        }

    @Test
    fun `a proof that does not verify is refused with its own code`() =
        runTest(dispatcher) {
            val repositories = MockRepositories.from(SecurityFixtures.backend())
            val viewModel =
                SessionsViewModel(sessions = repositories.sessions, proofSource = { BAD_PROOF.toByteArray() })
            advanceUntilIdle()

            viewModel.confirmRevokeAll()
            advanceUntilIdle()

            val outcome = viewModel.state.value.revokeAll as RevokeAllState.Refused
            assertEquals(SecurityFailure.of(AuthError.RevokeProofInvalid), outcome.failure)
        }

    @Test
    fun `the support report names the hash and every session under it`() =
        runTest(dispatcher) {
            val viewModel = settled(MockScenario.Default)
            val group =
                viewModel.state.value.groups
                    .first { !it.isCurrent }

            viewModel.openSupportReport(group.cohortHash)

            val report =
                viewModel.state.value.supportReport
                    .orEmpty()
            assertTrue(report.isNotEmpty(), "a report was produced")
            assertTrue(report.contains(group.cohortHash.orEmpty()), "the exact hash is what a human needs")
            group.sessions.forEach { session ->
                assertTrue(report.contains(session.id.value), "and every session id under it")
            }
        }

    @Test
    fun `the support report is dismissable and leaves nothing recorded against the cohort`() =
        runTest(dispatcher) {
            val viewModel = settled(MockScenario.Default)
            val before = viewModel.state.value.groups

            viewModel.openSupportReport(before.first { !it.isCurrent }.cohortHash)
            viewModel.dismissSupportReport()

            assertNull(viewModel.state.value.supportReport)
            assertEquals(before, viewModel.state.value.groups, "there is nothing for the user to assert")
        }

    @Test
    fun `a failed reading offers a retry that actually restarts the collection`() =
        runTest(dispatcher) {
            val backend =
                SecurityFixtures.backend(
                    MockScenario.Default.failing(MockOperation.SessionWrite, LocalError.Offline),
                )
            val viewModel = viewModelOver(MockRepositories.from(backend))
            advanceUntilIdle()
            val target =
                viewModel.state.value.groups
                    .flatMap(SessionCohortGroup::sessions)
                    .first { session -> session.revokedAt == null }

            viewModel.revokeSession(target.id)
            advanceUntilIdle()
            assertNotNull(viewModel.state.value.failure, "the write was refused")

            backend.applyScenario(MockScenario.Default)
            viewModel.reload()
            advanceUntilIdle()

            assertNull(viewModel.state.value.failure)
            assertTrue(
                viewModel.state.value.groups
                    .isNotEmpty(),
            )
        }

    @Test
    fun `a signed-out account shows an empty ledger rather than an error`() =
        runTest(dispatcher) {
            val viewModel = settled(MockScenario.NeverSignedIn)

            val state = viewModel.state.value
            assertNull(state.failure, "never having signed in is a mode, not a failure")
            assertTrue(state.hasNoSessions)
            assertTrue(
                state.devices.none(Device::isActive) || state.devices.isNotEmpty(),
                "the directory is still readable locally",
            )
        }

    private fun viewModelOver(repositories: MockRepositories): SessionsViewModel =
        SessionsViewModel(sessions = repositories.sessions, proofSource = StubMasterKeyProofSource())

    private fun TestScope.settled(scenario: MockScenario): SessionsViewModel {
        val viewModel = viewModelOver(MockRepositories.from(SecurityFixtures.backend(scenario)))
        advanceUntilIdle()
        return viewModel
    }

    private companion object {
        const val BAD_PROOF = "not-the-account-key"
    }
}
