package com.justin13888.capsule.feature.viewer

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.feature.viewer.media.VIDEO_CONTROLS_TAG
import kotlinx.collections.immutable.persistentListOf
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

/**
 * What the viewer draws, state by state.
 *
 * The assertions that earn their keep are about the five degraded conditions staying
 * *visually distinct*: each gets its own banner with its own handle, so a change that
 * collapsed two of them into one message fails here rather than reaching a user who then
 * cannot tell a photograph still uploading from one this device cannot decode.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ViewerScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private var retried = false
    private var refetched = false
    private var reviewed = false

    @Test
    fun `the loading state draws the stage rather than an empty window`() {
        renderStage(mutableStateOf(ViewerUiState.Loading))

        compose.onNodeWithTag(VIEWER_STAGE_TAG).assertIsDisplayed()
    }

    @Test
    fun `an asset that is gone reads as empty, with an explanation`() {
        renderStage(mutableStateOf(ViewerUiState.Missing))

        compose.onNodeWithTag(VIEWER_STAGE_TAG).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.viewer_empty_title)).assertIsDisplayed()
    }

    @Test
    fun `a failed listing localizes its code and offers a retry that fires`() {
        val failure = ViewerFailure(code = "error.sync.unauthenticated", detail = "sign in again")
        renderStage(mutableStateOf(ViewerUiState.Failed(failure)))

        compose.onNodeWithText(string(R.string.error_sync_unauthenticated)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.viewer_action_retry)).performClick()
        compose.waitForIdle()

        assertTrue("the error state must offer a remediation that actually runs", retried)
    }

    @Test
    fun `the stage names the rung it is drawing`() {
        renderStage(mutableStateOf(readyState()))

        compose.onNodeWithTag(VIEWER_TIER_BADGE_TAG).assertIsDisplayed()
    }

    @Test
    fun `each degraded condition gets its own banner`() {
        val state = mutableStateOf<ViewerUiState>(readyState())
        renderControls(state)

        conditions().forEach { (condition, degraded) ->
            state.value = degraded
            compose.waitForIdle()
            compose.onNodeWithTag(degradationTag(condition)).assertIsDisplayed()
        }
    }

    @Test
    fun `an undegraded asset raises no banner at all`() {
        renderControls(mutableStateOf(readyState()))

        ViewerDegradation.entries.forEach { condition ->
            compose.onNodeWithTag(degradationTag(condition)).assertDoesNotExist()
        }
    }

    @Test
    fun `the quarantine banner routes to the surface holding the refused change`() {
        renderControls(mutableStateOf(landmarkState(ViewerFixture.LANDMARKS.quarantinedAssetId)))

        compose.onNodeWithText(string(R.string.viewer_degraded_quarantined_action)).performClick()
        compose.waitForIdle()

        assertTrue("a refused change has to be reachable from where it is reported", reviewed)
    }

    @Test
    fun `the degraded state offers a retry rather than inviting a thrash`() {
        renderControls(mutableStateOf(degradedState()))

        compose.onNodeWithText(string(R.string.viewer_action_retry)).performClick()
        compose.waitForIdle()

        assertTrue("the degrade banner's action must re-run the fetch that walked down", refetched)
    }

    @Test
    fun `an asset from a newer peer disables editing without hiding it`() {
        renderControls(mutableStateOf(landmarkState(ViewerFixture.LANDMARKS.forwardSchemaAssetId)))

        compose.onNodeWithText(string(R.string.viewer_action_edit)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.viewer_action_edit)).assertIsNotEnabled()
    }

    @Test
    fun `an ordinary asset keeps editing available`() {
        renderControls(mutableStateOf(readyState()))

        compose.onNodeWithText(string(R.string.viewer_action_edit)).assertIsEnabled()
    }

    @Test
    fun `a clip gets a transport strip and a still does not`() {
        val video = requireNotNull(ViewerFixture.LIBRARY.assetsById[ViewerFixture.LANDMARKS.videoAssetId])
        val state = mutableStateOf<ViewerUiState>(readyState(video))
        renderControls(state)

        compose.onNodeWithTag(VIDEO_CONTROLS_TAG).assertIsDisplayed()

        state.value = readyState()
        compose.waitForIdle()
        compose.onNodeWithTag(VIDEO_CONTROLS_TAG).assertDoesNotExist()
    }

    /** One state per documented condition, each reached through a fixture landmark. */
    private fun conditions(): List<Pair<ViewerDegradation, ViewerUiState>> =
        listOf(
            ViewerDegradation.FullResolutionUnavailable to degradedState(),
            ViewerDegradation.OriginalOnAnotherDevice to awaitingState(),
            ViewerDegradation.UnreadableHere to landmarkState(ViewerFixture.LANDMARKS.unreadableAssetId),
            ViewerDegradation.CreatedByNewerVersion to landmarkState(ViewerFixture.LANDMARKS.forwardSchemaAssetId),
            ViewerDegradation.Quarantined to landmarkState(ViewerFixture.LANDMARKS.quarantinedAssetId),
        )

    private fun renderStage(state: MutableState<ViewerUiState>) {
        compose.setContent {
            CapsulePreviewSurface {
                Box(Modifier.fillMaxSize()) {
                    ViewerStage(state = state.value, actions = actions(), stage = ViewerStageState())
                }
            }
        }
        compose.waitForIdle()
    }

    private fun renderControls(state: MutableState<ViewerUiState>) {
        compose.setContent {
            CapsulePreviewSurface {
                ViewerControls(
                    state = state.value,
                    actions = actions(),
                    navigation = ViewerNavigation({}, {}, {}),
                    stage = ViewerStageState(),
                )
            }
        }
        compose.waitForIdle()
    }

    private fun actions() =
        ViewerActions(
            onRetry = { retried = true },
            onOpenPage = {},
            onLoadFullResolution = { refetched = true },
            onReviewQuarantine = { reviewed = true },
            onUpdateApp = {},
        )

    private fun string(key: Int): String = RuntimeEnvironment.getApplication().getString(key)

    private fun readyState(asset: Asset = ViewerFixture.ordinaryAsset()): ViewerUiState.Ready =
        ViewerUiState.Ready(
            pages = persistentListOf(ViewerPage.of(asset)),
            index = 0,
            representation = RepresentationStatus.Held(RepresentationTier.Preview),
        )

    private fun degradedState(): ViewerUiState.Ready =
        readyState().copy(
            representation =
                RepresentationStatus.Degraded(
                    wanted = RepresentationTier.Original,
                    held = RepresentationTier.Lqip,
                ),
        )

    private fun awaitingState(): ViewerUiState.Ready {
        val pending = ViewerFailure(code = "error.blob.pending_upload", detail = "not uploaded yet")
        return readyState().copy(representation = RepresentationStatus.AwaitingOriginal(pending))
    }

    private fun landmarkState(id: AssetId?): ViewerUiState.Ready =
        readyState(requireNotNull(ViewerFixture.LIBRARY.assetsById[requireNotNull(id)]))
}
