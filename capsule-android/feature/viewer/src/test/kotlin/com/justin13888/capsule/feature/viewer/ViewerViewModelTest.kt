package com.justin13888.capsule.feature.viewer

import app.cash.turbine.test
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.RepresentationTier
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

/**
 * The viewer's state machine, driven through the seeded repositories.
 *
 * The cases worth having are the ones where a fetch does not simply succeed. Three different
 * things can go wrong above the held rung and the design insists they stay apart: a permanent
 * `410` walks down the ladder and keeps drawing, a pending original is transient and comes
 * back on its own, and a `403` is an authorization change rather than a durability loss.
 * Reducing any of them to "failed" would tell the user a photograph was gone when it is not,
 * so each is asserted as its own state.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ViewerViewModelTest {
    private val landmarks = ViewerFixture.LANDMARKS

    @BeforeEach
    fun installTestDispatcher() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @AfterEach
    fun removeTestDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `opening at an asset lands on it inside the listing`() =
        runTest {
            val asset = ViewerFixture.ordinaryAsset()

            ViewerFixture.viewerViewModel(asset.id).state.test {
                val ready = awaitReady()
                assertEquals(asset.id, ready.current.asset.id)
                assertTrue(ready.pages.size > 1) { "the viewer should be able to swipe to neighbours" }
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an asset that is not in the library reads as empty rather than as an error`() =
        runTest {
            val absent = AssetId("00000000-0000-7000-8000-000000000000")

            ViewerFixture.viewerViewModel(absent).state.test {
                assertEquals(ViewerUiState.Missing, awaitFirst<ViewerUiState.Missing>())
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a permanently unreachable rung degrades down the ladder and keeps drawing`() =
        runTest {
            val id = ViewerFixture.require(landmarks.degradedAssetId, "degraded")
            val repositories = ViewerFixture.repositories(MockScenario.GoneDegrade)

            ViewerFixture.viewerViewModel(id, repositories).state.test {
                val degraded = awaitStatus<RepresentationStatus.Degraded>()
                assertEquals(RepresentationTier.Preview, degraded.wanted)
                assertEquals(RepresentationTier.Lqip, degraded.held)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a degraded fetch raises the full-resolution condition without touching the record`() =
        runTest {
            val id = ViewerFixture.require(landmarks.degradedAssetId, "degraded")
            val repositories = ViewerFixture.repositories(MockScenario.GoneDegrade)
            val viewModel = ViewerFixture.viewerViewModel(id, repositories)

            viewModel.state.test {
                awaitStatus<RepresentationStatus.Degraded>()
                val ready = viewModel.state.value as ViewerUiState.Ready
                assertTrue(ViewerDegradation.FullResolutionUnavailable in ready.degradations)
                assertEquals(id, ready.current.asset.id, "the asset must stay listed")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an original still uploading elsewhere is pending, not a failure`() =
        runTest {
            val id = ViewerFixture.require(landmarks.awaitingOriginalAssetId, "awaiting-original")
            val repositories = ViewerFixture.repositories(MockScenario.AwaitingOriginal)
            val viewModel = ViewerFixture.viewerViewModel(id, repositories)

            viewModel.state.test {
                awaitReady()
                viewModel.loadFullResolution()
                awaitStatus<RepresentationStatus.AwaitingOriginal>()
                val ready = viewModel.state.value as ViewerUiState.Ready
                assertTrue(ViewerDegradation.OriginalOnAnotherDevice in ready.degradations)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an authorization change is refused with its code rather than degraded`() =
        runTest {
            val id = ViewerFixture.require(landmarks.degradedAssetId, "degraded")
            val repositories = ViewerFixture.repositories(MockScenario.MembershipResync)

            ViewerFixture.viewerViewModel(id, repositories).state.test {
                val refused = awaitStatus<RepresentationStatus.Refused>()
                assertEquals("error.album.not_available", refused.failure.code)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `offline surfaces a refusal that knows it is offline`() =
        runTest {
            val id = ViewerFixture.require(landmarks.degradedAssetId, "degraded")
            val repositories = ViewerFixture.repositories(MockScenario.Offline)

            ViewerFixture.viewerViewModel(id, repositories).state.test {
                val refused = awaitStatus<RepresentationStatus.Refused>()
                assertTrue(refused.failure.isOffline) { "a dead link has no catalogue code to localize" }
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `paging moves the index and re-resolves the new page`() =
        runTest {
            val asset = ViewerFixture.ordinaryAsset()
            val viewModel = ViewerFixture.viewerViewModel(asset.id)

            viewModel.state.test {
                val ready = awaitReady()
                viewModel.openPage(ready.index + 1)
                val moved = viewModel.state.value as ViewerUiState.Ready
                assertEquals(ready.index + 1, moved.index)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `paging past the end is clamped rather than refused`() =
        runTest {
            val asset = ViewerFixture.ordinaryAsset()
            val viewModel = ViewerFixture.viewerViewModel(asset.id)

            viewModel.state.test {
                val ready = awaitReady()
                viewModel.openPage(Int.MAX_VALUE)
                val moved = viewModel.state.value as ViewerUiState.Ready
                assertEquals(ready.pages.size - 1, moved.index)
                cancelAndIgnoreRemainingEvents()
            }
        }
}
