package com.justin13888.capsule.feature.recovery

import app.cash.turbine.test
import com.justin13888.capsule.core.data.EscrowError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockScenario
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
 * The export screen's state machine.
 *
 * The interesting case is the boring one: an export is a long write to somewhere the user
 * named, and the screen has to stay honest about which of the three states it is in without
 * ever claiming to have written an archive it did not.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class BackupExportViewModelTest {
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
    fun `it opens loading and settles on the archives this device knows about`() =
        runTest(dispatcher) {
            val viewModel = BackupExportViewModel(RecoveryFixtures.repositories().recovery)

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the first state is loading, before anything is read")
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertNull(settled.failure)
                assertTrue(settled.backups.isNotEmpty(), "the seeded library has been exported once")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `naming nowhere says so rather than writing to nowhere`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.export()
            advanceUntilIdle()

            assertTrue(viewModel.state.value.destinationMissing)
            assertEquals(ExportPhase.Editing, viewModel.state.value.phase, "nothing was attempted")
        }

    @Test
    fun `an export walks working to done and reports what it wrote`() =
        runTest(dispatcher) {
            val viewModel = settled()
            viewModel.onDestinationChange(DESTINATION)

            viewModel.state.test {
                assertEquals(ExportPhase.Editing, awaitItem().phase)
                viewModel.export()
                assertEquals(ExportPhase.Working, awaitItem().phase)
                val done = awaitItem()
                assertEquals(ExportPhase.Done, done.phase)
                assertNotNull(done.exported)
                assertTrue(done.exported!!.entryCount > 0, "an archive that lists nothing restores nothing")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a refused write surfaces its catalogue code and keeps the destination`() =
        runTest(dispatcher) {
            val scenario = MockScenario.Default.failing(MockOperation.RecoveryWrite, EscrowError.Malformed)
            val viewModel = settled(scenario)
            viewModel.onDestinationChange(DESTINATION)

            viewModel.export()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(ExportPhase.Failed, state.phase)
            assertEquals(EscrowError.Malformed.code, state.failure?.code)
            assertEquals(DESTINATION, state.destination)

            viewModel.reset()
            assertEquals(ExportPhase.Editing, viewModel.state.value.phase)
            assertNull(viewModel.state.value.failure)
        }

    private fun TestScope.settled(scenario: MockScenario = MockScenario.Default): BackupExportViewModel =
        BackupExportViewModel(RecoveryFixtures.repositories(scenario).recovery).also {
            advanceUntilIdle()
            check(!it.state.value.isLoading) { "the fixture should settle without latency" }
        }

    private companion object {
        const val DESTINATION = "/sdcard/Download/capsule-backup.tar"
    }
}
