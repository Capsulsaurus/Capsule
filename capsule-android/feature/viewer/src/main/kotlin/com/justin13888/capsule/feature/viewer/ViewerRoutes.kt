package com.justin13888.capsule.feature.viewer

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorActions
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorScreen
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorViewModel
import com.justin13888.capsule.feature.viewer.info.AssetInfoPanel
import com.justin13888.capsule.feature.viewer.info.AssetInfoViewModel
import com.justin13888.capsule.feature.viewer.provenance.ProvenanceScreen
import com.justin13888.capsule.feature.viewer.provenance.ProvenanceViewModel
import org.koin.androidx.compose.koinViewModel
import org.koin.core.parameter.parametersOf

/**
 * Which asset a viewer was opened on, and inside what.
 *
 * One value rather than two loose strings because both reach the graph through a
 * `ParametersHolder`, which resolves by type: two `String`s there would be ambiguous, and the
 * ambiguity would resolve silently and wrongly rather than failing.
 */
@Immutable
data class ViewerArgs(
    /** The asset to open at. */
    val assetId: String,
    /**
     * The album the viewer was opened from, so paging stays inside the listing the user was
     * looking at rather than wandering the whole library.
     */
    val contextId: String? = null,
)

/**
 * The paged viewer, wired to its view model.
 *
 * Resolved once and handed back whole because the shell splits the viewer in two: the photo
 * is the main pane of a supporting-pane scaffold and, in tabletop posture, sits above a
 * physical hinge while the control strip sits below it. Sharing one state and one gesture
 * holder is what stops the two halves disagreeing about what is on screen.
 *
 * The two navigation callbacks are parameters rather than fields the caller fills in later:
 * reviewing a quarantine and opening a store listing are the shell's to route, and a feature
 * module never sees the destination vocabulary.
 *
 * @param args which asset to open, and inside what listing.
 * @param onReviewQuarantine opens the surface holding a refused change.
 * @param onUpdateApp opens the store listing for a build behind the peer that wrote the asset.
 */
@Composable
fun rememberViewerState(
    args: ViewerArgs,
    onReviewQuarantine: () -> Unit,
    onUpdateApp: () -> Unit,
): ViewerPresentation {
    val viewModel = koinViewModel<ViewerViewModel>(key = args.assetId) { parametersOf(args) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val pageKey =
        (state as? ViewerUiState.Ready)
            ?.current
            ?.asset
            ?.id
            ?.value
    val stage = rememberViewerStageState(pageKey)
    val actions =
        remember(viewModel, onReviewQuarantine, onUpdateApp) {
            ViewerActions(
                onRetry = viewModel::retry,
                onOpenPage = viewModel::openPage,
                onLoadFullResolution = viewModel::loadFullResolution,
                onReviewQuarantine = onReviewQuarantine,
                onUpdateApp = onUpdateApp,
            )
        }
    return ViewerPresentation(state, actions, stage)
}

/** Everything the shell needs to draw a viewer, once its view model has been resolved. */
@Stable
class ViewerPresentation(
    /** The immutable state the screens render. */
    val state: ViewerUiState,
    /** What the viewer can be asked to do. */
    val actions: ViewerActions,
    /** The zoom and frame the photo and its strip share. */
    val stage: ViewerStageState,
)

/** The information panel, wired to its view model. Serves both a side pane and a screen. */
@Composable
fun AssetInfoRoute(
    assetId: String,
    modifier: Modifier = Modifier,
) {
    val viewModel = koinViewModel<AssetInfoViewModel>(key = "info-$assetId") { parametersOf(assetId) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    AssetInfoPanel(state = state, onRetry = viewModel::retry, modifier = modifier)
}

/** The signed provenance chain, wired to its view model. */
@Composable
fun ProvenanceRoute(
    assetId: String,
    modifier: Modifier = Modifier,
) {
    val viewModel = koinViewModel<ProvenanceViewModel>(key = "provenance-$assetId") { parametersOf(assetId) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    ProvenanceScreen(state = state, onRetry = viewModel::retry, modifier = modifier)
}

/** The metadata editor, wired to its view model. */
@Composable
fun MetadataEditorRoute(
    assetId: String,
    modifier: Modifier = Modifier,
) {
    val viewModel = koinViewModel<MetadataEditorViewModel>(key = "editor-$assetId") { parametersOf(assetId) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions = remember(viewModel) { editorActions(viewModel) }
    MetadataEditorScreen(state = state, actions = actions, modifier = modifier)
}

/** Binds the editor's mutators to the value the screen takes. */
private fun editorActions(viewModel: MetadataEditorViewModel): MetadataEditorActions =
    MetadataEditorActions(
        onSetCaption = viewModel::setCaption,
        onClearCaption = viewModel::clearCaption,
        onRestoreCaption = viewModel::restoreCaption,
        onSetRating = viewModel::setRating,
        onSetCull = viewModel::setCull,
        onSetHidden = viewModel::setHidden,
        onAddUserTag = viewModel::addUserTag,
        onRemoveUserTag = viewModel::removeUserTag,
        onPromoteAiTag = viewModel::promoteAiTag,
        onDismissAiTag = viewModel::dismissAiTag,
        onConfirmLocation = viewModel::confirmLocation,
        onClearLocation = viewModel::clearLocation,
        onDismissRefusal = viewModel::dismissRefusal,
        onRetry = viewModel::retry,
    )
