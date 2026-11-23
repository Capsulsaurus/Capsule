package com.justin13888.capsule.feature.timeline

import app.cash.turbine.test
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.DeletionState
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.core.ui.gallery.GalleryCellSync
import com.justin13888.capsule.feature.timeline.cull.CullFilter
import com.justin13888.capsule.feature.timeline.cull.CullFrame
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
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
 * The state machine behind the library listing.
 *
 * Everything here runs against the seeded fakes rather than hand-rolled doubles, because the
 * interesting questions — does hiding fifty assets actually take them out of the timeline, does
 * a delete carry the retention window the settings ask for — are questions about how the
 * repositories behave together, and a stub that answered them by construction would prove
 * nothing.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class TimelineViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun installMainDispatcher() = Dispatchers.setMain(dispatcher)

    @AfterEach
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private suspend fun ready(viewModel: TimelineViewModel): TimelineUiState.Ready =
        viewModel.uiState.first { it is TimelineUiState.Ready } as TimelineUiState.Ready

    @Test
    fun `the library becomes a geometry of one section per populated day`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()

            val state = ready(viewModel)
            val geometry = state.geometry
            assertTrue(geometry.assetCount > 0)
            assertEquals(geometry.days.size, geometry.sectionStarts.size)
            assertEquals(0, geometry.sectionStarts.first())
            // The planner refuses a repeated section start, so an empty day must never reach it.
            assertTrue(geometry.days.all { it.count > 0 })
            assertTrue(
                geometry.sectionStarts
                    .toList()
                    .zipWithNext()
                    .all { (a, b) -> b > a },
            )
            assertNull(state.emptyReason)
        }

    @Test
    fun `the window the grid reports is what gets read`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()

            viewModel.onVisibleRange(400, 420)
            advanceUntilIdle()

            val state = ready(viewModel)
            assertNotNull(state.cellAt(400), "the reported window must be in hand")
            assertNotNull(state.cellAt(419))
            // The map holds the window and lets the rest go; a map that grew with the scroll
            // would keep the whole library resident.
            assertNull(state.cellAt(0), "a window far behind the viewport must not be retained")
        }

    @Test
    fun `a first-run library says nothing has been imported yet`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories(MockScenario.FirstRun)
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()

            val state = ready(viewModel)
            assertTrue(state.geometry.isEmpty)
            assertEquals(TimelineEmptyReason.NeverImported, state.emptyReason)
        }

    @Test
    fun `a library nobody ever signed in to says so rather than blaming the network`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories(MockScenario.NeverSignedIn)
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()

            assertEquals(TimelineEmptyReason.NeverImportedLocalOnly, ready(viewModel).emptyReason)
        }

    @Test
    fun `an empty library whose photographs are all in the trash does not claim there are none`() =
        runTest(dispatcher) {
            // Populated, so the trash keeps its contents, but every visible asset hidden.
            val mock = TimelineFixture.repositories()
            mock.backend.mutate { state ->
                state.assets.keys.toList().forEach { id ->
                    val asset = state.assets.getValue(id)
                    if (asset.deletion is DeletionState.Live) state.put(asset.copy(isHidden = true))
                }
            }
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()

            val state = ready(viewModel)
            assertTrue(state.geometry.isEmpty)
            assertEquals(TimelineEmptyReason.EverythingFiltered, state.emptyReason)
        }

    @Test
    fun `offline is reported so the toolbar can refuse what needs a server`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories(MockScenario.Offline)
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()

            assertTrue(ready(viewModel).isOffline)
        }

    @Test
    fun `a refused read surfaces its stable code, and retry re-reads`() =
        runTest(dispatcher) {
            val failing = MockScenario.Default.failing(MockOperation.TimelineRead, QuotaError.GraceLocked)
            val mock = TimelineFixture.repositories(failing)
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()

            val failed = viewModel.uiState.value
            assertTrue(failed is TimelineUiState.Failed)
            assertEquals(QuotaError.GraceLocked.code, (failed as TimelineUiState.Failed).code)

            mock.applyScenario(MockScenario.Default)
            viewModel.retry()
            advanceUntilIdle()

            assertTrue(viewModel.uiState.value is TimelineUiState.Ready)
        }

    @Test
    fun `hiding a selection takes it out of the timeline`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            val before = ready(viewModel).geometry.assetCount

            viewModel.apply(TimelineSelectionAction.Hide, intArrayOf(0, 1, 2))
            advanceUntilIdle()

            val after = ready(viewModel)
            assertEquals(before - 3, after.geometry.assetCount)
            assertEquals(TimelineSelectionAction.Hide, after.pendingAction?.action)
            assertNull(after.pendingAction?.failure)
        }

    @Test
    fun `deleting a selection signs the retention window the library asks for`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            val doomed = mock.timeline.timelineWindow(0, 2).map { it.id }
            val retention =
                mock.settings
                    .settings()
                    .first()
                    .defaultRetention

            viewModel.apply(TimelineSelectionAction.Delete, intArrayOf(0, 1))
            advanceUntilIdle()

            doomed.forEach { id ->
                val deletion = mock.assets.findByUuid(id)?.deletion
                assertTrue(deletion is DeletionState.Trashed, "${id.value} should be in the trash")
                val trashed = deletion as DeletionState.Trashed
                // The deadline travels with the deletion; nothing downstream can shorten it.
                assertEquals(TimelineFixture.CLOCK.instant().plus(retention), trashed.retentionUntil)
            }
        }

    @Test
    fun `favouriting raises a selection to the top of the scale and pressing again clears it`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            val ids = mock.timeline.timelineWindow(0, 3).map { it.id }

            viewModel.apply(TimelineSelectionAction.Favourite, intArrayOf(0, 1, 2))
            advanceUntilIdle()
            assertTrue(ids.all { mock.assets.findByUuid(it)?.rating == FAVOURITE_RATING })

            viewModel.apply(TimelineSelectionAction.Favourite, intArrayOf(0, 1, 2))
            advanceUntilIdle()
            assertTrue(ids.all { mock.assets.findByUuid(it)?.rating == 0 })
        }

    @Test
    fun `sharing parks the resolved ids for the app module instead of acting on them`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            val expected = mock.timeline.timelineWindow(0, 2).map { it.id.value }

            viewModel.apply(TimelineSelectionAction.Share, intArrayOf(0, 1))
            advanceUntilIdle()

            val handoff = ready(viewModel).handoff
            assertEquals(TimelineHandoff.Target.Share, handoff?.target)
            assertEquals(expected, handoff?.assetIds)

            viewModel.onHandoffHandled()
            advanceUntilIdle()
            assertNull(ready(viewModel).handoff, "a routed hand-off must not be routable twice")
        }

    @Test
    fun `culling a day opens a review over exactly that day`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            val geometry = ready(viewModel).geometry
            val day = geometry.days[1]

            viewModel.reviewSection(1)
            advanceUntilIdle()

            val culling = requireNotNull(ready(viewModel).culling)
            assertEquals(day.count, culling.counts.total)
            assertEquals(day.startOffset, culling.frames.first().asset)
            assertEquals(day.endOffsetExclusive - 1, culling.frames.last().asset)
        }

    @Test
    fun `flagging the frame under review writes the flag and moves on`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            viewModel.reviewSection(0)
            advanceUntilIdle()
            val first = requireNotNull(ready(viewModel).culling?.current)

            viewModel.flagCurrent(CullFlag.Reject)
            advanceUntilIdle()

            val culling = requireNotNull(ready(viewModel).culling)
            assertEquals(CullFlag.Reject, culling.frames.first().flag)
            assertEquals(1, culling.position, "a review pass is one keystroke per frame")
            assertEquals(
                CullFlag.Reject,
                mock.assets
                    .findByUuid(first.assetId)
                    ?.cull
                    ?.knownOrNull,
                "the optimistic flag must be followed by the real write",
            )
        }

    @Test
    fun `the review filter is exact over the set it was opened with`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            viewModel.reviewSection(0)
            advanceUntilIdle()

            viewModel.flagCurrent(CullFlag.Reject)
            advanceUntilIdle()
            viewModel.setCullFilter(CullFilter.Rejects)
            advanceUntilIdle()

            val culling = requireNotNull(ready(viewModel).culling)
            assertEquals(1, culling.visible.size)
            assertTrue(culling.visible.all { it.flag == CullFlag.Reject })
        }

    @Test
    fun `the sweep trashes exactly the rejected frames and drops them from the review`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            viewModel.reviewSection(0)
            advanceUntilIdle()

            viewModel.flagCurrent(CullFlag.Reject)
            viewModel.flagCurrent(CullFlag.Pick)
            viewModel.flagCurrent(CullFlag.Reject)
            advanceUntilIdle()
            val culling = requireNotNull(ready(viewModel).culling)
            val rejected = culling.rejectedAssets()
            val kept = culling.frames.map(CullFrame::assetId) - rejected.toSet()
            assertEquals(2, rejected.size)

            viewModel.confirmSweep(true)
            assertTrue(requireNotNull(ready(viewModel).culling).isConfirmingSweep)
            viewModel.sweepRejects()
            advanceUntilIdle()

            rejected.forEach {
                assertTrue(
                    mock.assets.findByUuid(it)?.deletion is DeletionState.Trashed,
                    "${it.value} was rejected and should have been swept",
                )
            }
            kept.forEach {
                assertFalse(
                    mock.assets.findByUuid(it)?.deletion is DeletionState.Trashed,
                    "${it.value} was not rejected and must not have moved",
                )
            }
            val after = requireNotNull(ready(viewModel).culling)
            assertTrue(after.frames.none { it.assetId in rejected })
            assertFalse(after.isConfirmingSweep)
        }

    @Test
    fun `leaving the review returns the grid`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            viewModel.reviewSection(0)
            advanceUntilIdle()
            assertNotNull(ready(viewModel).culling)

            viewModel.closeReview()
            advanceUntilIdle()

            assertNull(ready(viewModel).culling)
        }

    @Test
    fun `a refused write is reported with its code rather than silently doing nothing`() =
        runTest(dispatcher) {
            val mock: MockRepositories = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            mock.applyScenario(
                MockScenario.Default.failing(MockOperation.AssetWrite, QuotaError.GraceLocked),
            )

            viewModel.apply(TimelineSelectionAction.Hide, intArrayOf(0))
            advanceUntilIdle()

            val outcome = ready(viewModel).pendingAction
            assertEquals(TimelineSelectionAction.Hide, outcome?.action)
            assertEquals(QuotaError.GraceLocked.code, outcome?.failure)

            viewModel.onOutcomeHandled()
            advanceUntilIdle()
            assertNull(ready(viewModel).pendingAction, "an announced outcome must not repeat")
        }

    @Test
    fun `the state stream settles rather than churning once the library is read`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories()
            val viewModel = TimelineFixture.viewModel(mock)

            viewModel.uiState.test {
                assertEquals(TimelineUiState.Loading, awaitItem())
                advanceUntilIdle()
                val settled = expectMostRecentItem()
                assertTrue(settled is TimelineUiState.Ready)
                assertTrue((settled as TimelineUiState.Ready).cells.isNotEmpty())
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an outstanding upload is reported only when the window really holds one`() =
        runTest(dispatcher) {
            val mock = TimelineFixture.repositories(MockScenario.Offline)
            val viewModel = TimelineFixture.viewModel(mock)
            advanceUntilIdle()
            // The newest frames are the ones still in flight, and the grid opens on them.
            viewModel.onVisibleRange(0, 40)
            advanceUntilIdle()

            val expected =
                mock.timeline
                    .timelineWindow(0, 100)
                    .any { it.toTimelineCell().markers.sync == GalleryCellSync.Pending }
            assertEquals(expected, ready(viewModel).hasPendingUploads)
        }
}
