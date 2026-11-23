package com.justin13888.capsule.feature.viewer

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.RgbColor
import com.justin13888.capsule.feature.viewer.media.AssetRender
import com.justin13888.capsule.feature.viewer.media.AssetSurface
import com.justin13888.capsule.feature.viewer.media.ZoomPan

/** Test handle for the paging surface, so a test can find the photo without a semantics tree walk. */
const val VIEWER_STAGE_TAG: String = "viewer.stage"

/** Test handle for the chip naming which rung of the ladder is on screen. */
const val VIEWER_TIER_BADGE_TAG: String = "viewer.tier-badge"

/**
 * Everything the viewer can be asked to do, in one value.
 *
 * A holder rather than seven parameters on every screen: the same set is handed to the photo,
 * the control strip and each banner, and threading them individually would make adding one
 * a change at four call sites.
 */
@Stable
class ViewerActions(
    /** Re-reads the listing after a failure. */
    val onRetry: () -> Unit,
    /** Moves the pager to an index. */
    val onOpenPage: (Int) -> Unit,
    /** Fetches the original bytes, only ever on explicit demand. */
    val onLoadFullResolution: () -> Unit,
    /** Opens the surface holding a refused change. */
    val onReviewQuarantine: () -> Unit,
    /** Opens the store listing when this build is behind the peer that wrote the asset. */
    val onUpdateApp: () -> Unit,
)

/**
 * The photo itself, full bleed, and nothing that is not the photo.
 *
 * Chrome deliberately stays out: the strip, the banners and the metadata all live below —
 * in tabletop posture, below a physical hinge — because a photograph with controls floating
 * on it is a photograph the user cannot judge, which is the whole reason the viewer is
 * immersive in the first place. The one exception is the badge naming which rung of the
 * representation ladder is being drawn, which is a statement *about the pixels* and belongs
 * beside them.
 */
@Composable
fun ViewerStage(
    state: ViewerUiState,
    actions: ViewerActions,
    stage: ViewerStageState,
    modifier: Modifier = Modifier,
) {
    when (state) {
        ViewerUiState.Loading ->
            CapsuleLoadingState(
                modifier = modifier.fillMaxSize().testTag(VIEWER_STAGE_TAG),
                title = stringResource(R.string.viewer_loading),
                contentDescription = stringResource(R.string.common_loading),
            )

        ViewerUiState.Missing ->
            CapsuleEmptyState(
                title = stringResource(R.string.viewer_empty_title),
                modifier = modifier.fillMaxSize().testTag(VIEWER_STAGE_TAG),
                body = stringResource(R.string.viewer_empty_body),
                icon = Icons.Filled.PhotoLibrary,
            )

        is ViewerUiState.Failed -> FailedStage(state, actions, modifier)
        is ViewerUiState.Ready -> ReadyStage(state, actions, stage, modifier)
    }
}

/** The listing could not be read; the code is localized and a retry is offered. */
@Composable
private fun FailedStage(
    state: ViewerUiState.Failed,
    actions: ViewerActions,
    modifier: Modifier,
) {
    CapsuleErrorState(
        title = stringResource(R.string.viewer_error_title),
        modifier = modifier.fillMaxSize().testTag(VIEWER_STAGE_TAG),
        body = state.failure.localizedMessage(),
        action = {
            CapsuleButton(onClick = actions.onRetry, label = stringResource(R.string.viewer_action_retry))
        },
    )
}

/**
 * The pager.
 *
 * The pager's settled page and the view model's index are kept in step in both directions,
 * each guarded by an equality check. Without the guards the two would drive each other in a
 * loop; without the second direction, an index changed by anything other than a swipe — a
 * deep link, an asset vanishing underneath — would leave the photo on screen unchanged.
 */
@Composable
private fun ReadyStage(
    state: ViewerUiState.Ready,
    actions: ViewerActions,
    stage: ViewerStageState,
    modifier: Modifier,
) {
    val pager = rememberPagerState(initialPage = state.index) { state.pages.size }
    LaunchedEffect(pager.settledPage) {
        if (pager.settledPage != state.index) actions.onOpenPage(pager.settledPage)
    }
    LaunchedEffect(state.index) {
        if (pager.currentPage != state.index) pager.scrollToPage(state.index)
    }

    val pagerLabel = stringResource(R.string.viewer_a11y_pager)
    Box(
        modifier = modifier.fillMaxSize().testTag(VIEWER_STAGE_TAG),
        contentAlignment = Alignment.Center,
    ) {
        HorizontalPager(
            state = pager,
            modifier = Modifier.fillMaxSize().semantics { contentDescription = pagerLabel },
        ) { page ->
            StagePage(
                page = state.pages[page],
                stage = stage,
                isCurrent = page == state.index,
                position = page + 1,
                total = state.pages.size,
            )
        }
        TierBadge(
            state = state,
            modifier = Modifier.align(Alignment.TopEnd).padding(BadgeInset),
        )
    }
}

/** One page of the pager. Only the current page's gestures reach the shared stage state. */
@Composable
private fun StagePage(
    page: ViewerPage,
    stage: ViewerStageState,
    isCurrent: Boolean,
    position: Int,
    total: Int,
) {
    val description =
        stringResource(R.string.viewer_a11y_stage).withArguments("position" to position, "total" to total)
    AssetSurface(
        render = page.asset.toRender(),
        zoom = if (isCurrent) stage.zoom else ZoomPan.Fitted,
        frame = stage.frame,
        onFrameChange = { if (isCurrent) stage.measure(it) },
        onZoomChange = { if (isCurrent) stage.apply(it) },
        contentDescription = description,
    )
}

/**
 * The chip naming what is actually being drawn.
 *
 * Always present, not only when something is wrong. A user who can see at a glance that they
 * are looking at a screen-sized derivative never has to wonder whether the softness is the
 * lens or the ladder.
 */
@Composable
private fun TierBadge(
    state: ViewerUiState.Ready,
    modifier: Modifier,
) {
    val tier = drawableTier(state.current.asset)
    val degraded = ViewerDegradation.FullResolutionUnavailable in state.degradations
    CapsuleBadge(
        label = stringResource(tier.labelRes()),
        modifier = modifier.testTag(VIEWER_TIER_BADGE_TAG),
        tone = if (degraded) CapsuleBadgeTone.Degraded else CapsuleBadgeTone.Neutral,
        contentDescription = stringResource(tier.labelRes()),
    )
}

/**
 * What to draw for one asset.
 *
 * A record with no probed dimensions falls back to a square, which is the same fallback the
 * timeline's aspect codec makes — two views of the library disagreeing about the shape of the
 * same unreadable file would look like a layout bug rather than a missing decoder.
 */
internal fun Asset.toRender(): AssetRender =
    AssetRender(
        dominantColor = lqip?.dominantColor?.toComposeColor(),
        aspectRatio = dimensions?.aspectRatio ?: SQUARE,
        isUnreadable = isUnreadable(this),
    )

/** The sidecar's `dominant_color` as a Compose colour. */
private fun RgbColor.toComposeColor(): Color = Color(red = red, green = green, blue = blue)

/** The fallback shape for a file whose dimensions were never probed. */
private const val SQUARE = 1f

private val BadgeInset = 12.dp
