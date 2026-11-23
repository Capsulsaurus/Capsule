package com.justin13888.capsule.feature.viewer

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.FitScreen
import androidx.compose.material.icons.filled.HighQuality
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.filled.ZoomIn
import androidx.compose.material.icons.filled.ZoomOut
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.feature.viewer.media.VideoControls
import com.justin13888.capsule.feature.viewer.media.rememberPlaybackState

/** Test handle for the whole control strip, which tabletop posture moves below the hinge. */
const val VIEWER_CONTROLS_TAG: String = "viewer.controls"

/** Test handle for the banner raised when a tier fetch was refused outright. */
const val VIEWER_REFUSAL_TAG: String = "viewer.refusal"

/** Where the viewer's chrome sends the user next. */
@Stable
class ViewerNavigation(
    /** Opens the information panel — a pane beside the photo, or a screen of its own. */
    val onOpenInfo: () -> Unit,
    /** Opens the signed provenance chain. */
    val onOpenProvenance: () -> Unit,
    /** Opens the metadata editor. */
    val onOpenEditor: () -> Unit,
)

/**
 * The strip the user operates the viewer with.
 *
 * Separated from the photo rather than overlaid on it, which is what makes the tabletop
 * arrangement possible at all: an overlay has nowhere to go when the window splits in half.
 * It scrolls, because at 200% font scale on a compact window the banners alone can be taller
 * than the space below a hinge.
 */
@Composable
fun ViewerControls(
    state: ViewerUiState,
    actions: ViewerActions,
    navigation: ViewerNavigation,
    stage: ViewerStageState,
    modifier: Modifier = Modifier,
) {
    val ready = state as? ViewerUiState.Ready ?: return
    Column(
        modifier =
            modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(StripPadding)
                .testTag(VIEWER_CONTROLS_TAG),
        verticalArrangement = Arrangement.spacedBy(StripSpacing),
    ) {
        DegradationBanners(
            degradations = ready.degradations,
            heldTier = drawableTier(ready.current.asset),
            deviceLabel = ready.originDeviceLabel,
            formatToken =
                ready.current.asset.contentType
                    ?.raw,
            onAction = { actions.dispatch(it) },
        )
        RefusalBanner(ready.representation, actions)
        PositionAndZoom(ready, stage)
        ClipControls(ready)
        ActionRow(ready, actions, navigation)
    }
}

/** Turns a banner's chosen remediation into the callback that performs it. */
private fun ViewerActions.dispatch(action: DegradationAction) {
    when (action) {
        DegradationAction.Retry -> onLoadFullResolution()
        DegradationAction.UpdateApp -> onUpdateApp()
        DegradationAction.ReviewQuarantine -> onReviewQuarantine()
    }
}

/**
 * A tier fetch that was refused rather than degraded.
 *
 * Kept apart from the degraded banners because it is a different kind of statement: a
 * revoked album or a dead connection is something that can be retried and may come back,
 * whereas a degradation is the state the library is legitimately in.
 */
@Composable
private fun RefusalBanner(
    status: RepresentationStatus,
    actions: ViewerActions,
) {
    val refused = status as? RepresentationStatus.Refused ?: return
    CapsuleBanner(
        title = stringResource(R.string.viewer_degraded_access_changed_title),
        modifier = Modifier.fillMaxWidth().testTag(VIEWER_REFUSAL_TAG),
        body = refused.failure.localizedMessage(),
        severity = CapsuleBannerSeverity.Error,
        action = {
            CapsuleButton(
                onClick = actions.onLoadFullResolution,
                label = stringResource(R.string.viewer_action_retry),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        },
    )
}

/** Where in the listing the user is, and the zoom controls for the photo above. */
@Composable
private fun PositionAndZoom(
    state: ViewerUiState.Ready,
    stage: ViewerStageState,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(StripSpacing),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text =
                stringResource(R.string.viewer_a11y_stage).withArguments(
                    "position" to state.index + 1,
                    "total" to state.pages.size,
                ),
            style = MaterialTheme.typography.labelLarge,
        )
        val scale = ViewerFormat.scale(stage.zoom.scale)
        Text(
            text = stringResource(R.string.viewer_zoom_level).withArguments("scale" to scale),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    Row(horizontalArrangement = Arrangement.spacedBy(StripSpacing)) {
        CapsuleButton(
            onClick = stage::zoomOut,
            label = stringResource(R.string.viewer_zoom_out),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            icon = Icons.Filled.ZoomOut,
            enabled = !stage.zoom.isFitted,
        )
        CapsuleButton(
            onClick = stage::zoomIn,
            label = stringResource(R.string.viewer_zoom_in),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            icon = Icons.Filled.ZoomIn,
        )
        CapsuleButton(
            onClick = stage::fit,
            label = stringResource(R.string.viewer_zoom_reset),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            icon = Icons.Filled.FitScreen,
            enabled = !stage.zoom.isFitted,
        )
    }
}

/** The transport strip, present only when the open asset is a clip. */
@Composable
private fun ClipControls(state: ViewerUiState.Ready) {
    val duration = state.current.asset.duration ?: return
    val playback = rememberPlaybackState(duration)
    VideoControls(
        state = playback.state,
        onToggle = playback::toggle,
        onSeek = playback::seekTo,
    )
}

/**
 * The four doors out of the viewer.
 *
 * The editor is disabled rather than hidden when the asset came from a newer peer. Hiding it
 * would leave the user hunting for a control that exists everywhere else; disabling it beside
 * a banner that says exactly why is the difference between a limitation and a bug.
 */
@Composable
private fun ActionRow(
    state: ViewerUiState.Ready,
    actions: ViewerActions,
    navigation: ViewerNavigation,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(StripSpacing),
    ) {
        CapsuleButton(
            onClick = navigation.onOpenInfo,
            label = stringResource(R.string.viewer_action_info),
            emphasis = CapsuleButtonEmphasis.Secondary,
            icon = Icons.Filled.Info,
        )
        CapsuleButton(
            onClick = navigation.onOpenProvenance,
            label = stringResource(R.string.viewer_action_provenance),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            icon = Icons.Filled.History,
        )
        CapsuleButton(
            onClick = navigation.onOpenEditor,
            label = stringResource(R.string.viewer_action_edit),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            icon = Icons.Filled.Tune,
            enabled = state.current.isEditable,
        )
    }
    if (state.canRequestOriginal) {
        CapsuleButton(
            onClick = actions.onLoadFullResolution,
            label = stringResource(R.string.viewer_action_full_resolution),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            icon = Icons.Filled.HighQuality,
        )
    }
}

/**
 * Whether asking for the original is worth offering.
 *
 * Hidden once the original is in hand, and hidden while the ladder has already reported that
 * it cannot be reached — the design's rule is that the client never thrashes the fetch, and a
 * button inviting the user to do exactly that would be the UI's version of thrashing it.
 */
private val ViewerUiState.Ready.canRequestOriginal: Boolean
    get() =
        !current.asset.heldRepresentations.holds(RepresentationTier.Original) &&
            representation !is RepresentationStatus.Degraded &&
            representation !is RepresentationStatus.AwaitingOriginal

private val StripPadding = 16.dp
private val StripSpacing = 8.dp
