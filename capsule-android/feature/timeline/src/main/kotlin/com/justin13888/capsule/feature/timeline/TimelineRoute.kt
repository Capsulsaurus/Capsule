package com.justin13888.capsule.feature.timeline

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.feature.timeline.cull.CullingCallbacks
import org.koin.androidx.compose.koinViewModel

/**
 * The timeline, wired to its view model.
 *
 * This is the whole of what the app module calls: a destination replaces its placeholder with
 * one call here and hands over four navigation lambdas. Everything else — the index, the
 * windows, the selection writes, the culling pass — is behind [TimelineViewModel], and none of
 * it is reachable from the shell, which is what keeps the feature swappable and the shell
 * ignorant of the catalog.
 *
 * The route is a thin adapter over [TimelineScreen] on purpose: the screen takes state and
 * callbacks and nothing else, so every state it can be in is reachable from a preview and from
 * a test without Koin.
 *
 * @param onOpenAsset opens one asset in the viewer.
 * @param onImport opens the importer.
 * @param onShareAssets mints a share link over a selection. Sharing belongs to another product
 *   area, so the timeline resolves the ids and the app module routes them.
 * @param onAddToAlbum files a selection into an album, for the same reason.
 */
@Composable
fun TimelineRoute(
    onOpenAsset: (String) -> Unit,
    onImport: () -> Unit,
    onShareAssets: (List<String>) -> Unit,
    onAddToAlbum: (List<String>) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: TimelineViewModel = koinViewModel(),
) {
    val state by viewModel.uiState.collectAsStateWithLifecycle()

    // A hand-off is state rather than an event so a rotation mid-route cannot drop it. It is
    // cleared the instant it is acted on, so acting on it twice is not possible either.
    val handoff = (state as? TimelineUiState.Ready)?.handoff
    LaunchedEffect(handoff) {
        val pending = handoff ?: return@LaunchedEffect
        when (pending.target) {
            TimelineHandoff.Target.Share -> onShareAssets(pending.assetIds)
            TimelineHandoff.Target.AddToAlbum -> onAddToAlbum(pending.assetIds)
        }
        viewModel.onHandoffHandled()
    }

    val callbacks =
        remember(viewModel, onOpenAsset, onImport) {
            TimelineCallbacks(
                onOpenAsset = onOpenAsset,
                onImport = onImport,
                onRetry = viewModel::retry,
                onVisibleRange = viewModel::onVisibleRange,
                onSelectionAction = viewModel::apply,
                onReviewSection = viewModel::reviewSection,
                onOutcomeHandled = viewModel::onOutcomeHandled,
                culling =
                    CullingCallbacks(
                        onClose = viewModel::closeReview,
                        onFilter = viewModel::setCullFilter,
                        onFlag = viewModel::flagCurrent,
                        onMove = viewModel::moveReview,
                        onMoveTo = viewModel::moveReviewTo,
                        onConfirmSweep = viewModel::confirmSweep,
                        onSweep = viewModel::sweepRejects,
                    ),
            )
        }

    TimelineScreen(state = state, callbacks = callbacks, modifier = modifier)
}
