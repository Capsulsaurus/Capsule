package com.justin13888.capsule.feature.audit

import app.cash.turbine.test
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
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
 * The state machine behind both panes.
 *
 * Every case here is one the two-pane layout can put on screen at once, which is why the
 * filter and the selection are asserted together: a selection that survives a filter it no
 * longer matches is a detail pane showing a row the list does not contain.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AuditViewModelTest {
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
    fun `it opens loading and settles on the assembled log`() =
        runTest(dispatcher) {
            val viewModel = AuditViewModel(sourceOver(AuditFixtures.repositories()))

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the first state is loading, before anything is read")
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertNull(settled.failure)
                assertTrue(settled.visible.isNotEmpty(), "the seeded library has history to show")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `excluding an origin hides only that half of the log`() =
        runTest(dispatcher) {
            val viewModel = settled()

            viewModel.toggleCategory(AuditCategory.Moderation)

            val state = viewModel.state.value
            assertTrue(state.visible.isNotEmpty(), "the user's own writes are still listed")
            assertTrue(
                state.visible.none { it.category == AuditCategory.Moderation },
                "an excluded origin contributes nothing",
            )
        }

    @Test
    fun `excluding every origin says so instead of silently showing everything`() =
        runTest(dispatcher) {
            val viewModel = settled()

            AuditCategory.entries.forEach(viewModel::toggleCategory)

            assertTrue(viewModel.state.value.hasNoMatches, "an empty selection matches nothing, and says so")
            assertFalse(viewModel.state.value.isEmpty, "the library is not empty — the filter is too narrow")
        }

    @Test
    fun `clearing the filters puts every entry back`() =
        runTest(dispatcher) {
            val viewModel = settled()
            AuditCategory.entries.forEach(viewModel::toggleCategory)

            viewModel.clearFilters()

            assertTrue(viewModel.state.value.filter.isUnfiltered)
            assertEquals(viewModel.state.value.log.entries, viewModel.state.value.visible)
        }

    @Test
    fun `narrowing to this device keeps only what this device signed`() =
        runTest(dispatcher) {
            val viewModel = settled()
            val thisDevice = viewModel.state.value.thisDevice
            assertNotNull(thisDevice, "the seeded directory names the device the app runs on")

            viewModel.toggleThisDevice()

            val state = viewModel.state.value
            assertEquals(thisDevice, state.filter.onlyDevice)
            assertTrue(
                state.visible.all { it.signedBy == thisDevice },
                "an operator action is not signed by a device and cannot survive this filter",
            )
        }

    @Test
    fun `a selection that the filter excludes stops resolving, so the detail pane cannot lie`() =
        runTest(dispatcher) {
            val viewModel = settled()
            val moderation =
                viewModel.state.value.visible
                    .first { it.category == AuditCategory.Moderation }
            viewModel.select(moderation.id)
            assertNotNull(viewModel.state.value.selected)

            viewModel.toggleCategory(AuditCategory.Moderation)

            assertNull(viewModel.state.value.selected, "a hidden row has no record to show")
        }

    @Test
    fun `a refusal surfaces its catalogue code, and retrying recovers`() =
        runTest(dispatcher) {
            val backend = MockBackend(AuditFixtures.library(), AuditFixtures.clock(), MockScenario.Default)
            backend.applyScenario(MockScenario.Default.failing(MockOperation.TimelineRead, QuotaError.Exceeded))
            val viewModel = AuditViewModel(sourceOver(MockRepositories.from(backend)))
            viewModel.state.test {
                awaitItem()
                val failed = awaitItem()
                assertEquals(QuotaError.Exceeded.code, failed.failure?.code)

                backend.applyScenario(MockScenario.Default)
                viewModel.reload()

                assertTrue(awaitItem().isLoading)
                val recovered = awaitItem()
                assertNull(recovered.failure)
                assertTrue(recovered.visible.isNotEmpty())
                cancelAndIgnoreRemainingEvents()
            }
        }

    private fun settled(): AuditViewModel =
        AuditViewModel(sourceOver(AuditFixtures.repositories())).also { viewModel ->
            dispatcher.scheduler.advanceUntilIdle()
            check(!viewModel.state.value.isLoading) { "the fixture should settle without latency" }
        }

    private fun sourceOver(mock: MockRepositories): AuditLogSource =
        AuditLogSource(
            timeline = mock.timeline,
            assets = mock.assets,
            moderation = mock.moderation,
            sessions = mock.sessions,
            scanLimit = PAGE,
        )

    private companion object {
        const val PAGE = 40
    }
}
