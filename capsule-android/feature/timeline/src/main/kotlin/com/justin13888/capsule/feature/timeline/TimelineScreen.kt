package com.justin13888.capsule.feature.timeline

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material.icons.filled.RateReview
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.designsystem.component.capsuleTouchTarget
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.ui.gallery.rememberGalleryState
import com.justin13888.capsule.feature.timeline.cull.CullingCallbacks
import com.justin13888.capsule.feature.timeline.cull.CullingReview

private val StripInset = 12.dp

/**
 * Everything the timeline can ask the app module to do.
 *
 * Bundled rather than passed one by one so the screen stays previewable — a preview supplies
 * an empty holder and gets a real screen — and so that adding a route does not reopen every
 * call site. Every field is a lambda, which Compose treats as stable, so passing the holder
 * costs nothing in recomposition.
 *
 * Long by construction: this type exists *because* the screen has this many outward edges, and
 * collapsing them into fewer would mean either a sealed event type the screen has to switch on
 * or a screen that reaches for a view model. Both are worse than a list of named lambdas.
 */
@Suppress("LongParameterList")
@Immutable
class TimelineCallbacks(
    /** Opens one asset in the viewer. Receives the id as the navigation key carries it. */
    val onOpenAsset: (String) -> Unit = {},
    /** Opens the importer — the remediation on the never-imported empty state. */
    val onImport: () -> Unit = {},
    /** Re-reads the library after a failure. */
    val onRetry: () -> Unit = {},
    /** Reports which assets the solved plan says are on screen. */
    val onVisibleRange: (from: Int, until: Int) -> Unit = { _, _ -> },
    /** Applies a toolbar action to the selected indices. */
    val onSelectionAction: (TimelineSelectionAction, IntArray) -> Unit = { _, _ -> },
    /** Opens the culling review over one day. */
    val onReviewSection: (Int) -> Unit = {},
    /** Clears the one-shot outcome after it has been announced. */
    val onOutcomeHandled: () -> Unit = {},
    /** What the culling review can do. */
    val culling: CullingCallbacks = CullingCallbacks(),
)

/**
 * The library listing — the screen this application exists to show.
 *
 * Stateless: everything it draws comes from [state] and everything it does goes through
 * [callbacks], so every one of its states is reachable from a preview and from a test without
 * a view model, a repository, or a network.
 *
 * The culling review replaces the grid rather than sitting on top of it. Reviewing is a
 * full-attention mode — one photograph, one decision — and a grid visible behind it would be
 * an invitation to lose your place in the pass.
 */
@Composable
fun TimelineScreen(
    state: TimelineUiState,
    callbacks: TimelineCallbacks,
    modifier: Modifier = Modifier,
) {
    Box(modifier = modifier.fillMaxSize().testTag(TimelineTestTags.ROOT)) {
        when (state) {
            TimelineUiState.Loading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxSize().testTag(TimelineTestTags.LOADING),
                    title = stringResource(R.string.timeline_loading),
                    contentDescription = stringResource(R.string.timeline_loading),
                )

            is TimelineUiState.Failed -> TimelineErrorState(state = state, onRetry = callbacks.onRetry)

            is TimelineUiState.Ready ->
                if (state.culling != null) {
                    CullingReview(
                        state = state.culling,
                        callbacks = callbacks.culling,
                        modifier = Modifier.fillMaxSize(),
                    )
                } else {
                    TimelineLibrary(state = state, callbacks = callbacks)
                }
        }
    }
}

/**
 * The failure state.
 *
 * Shows the localized `error.*` sentence and a way out, never the raw code and never a
 * transport status: a status says nothing the reader can act on and two deployments may spell
 * the same condition differently.
 */
@Composable
private fun TimelineErrorState(
    state: TimelineUiState.Failed,
    onRetry: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.timeline_error_title),
        modifier = Modifier.fillMaxSize().testTag(TimelineTestTags.ERROR),
        body = localizedError(state.code),
        action = {
            CapsuleButton(onClick = onRetry, label = stringResource(R.string.timeline_error_retry))
        },
    )
}

/**
 * The grid, with whatever has to be said above it.
 *
 * The gallery's scroll state is hoisted to here rather than left inside the grid, because the
 * app bar's culling action is about the day *at the top of the viewport* — the one the user is
 * looking at. Anything else would make "review this day" mean "review whichever day the review
 * happened to start on".
 */
@Composable
private fun TimelineLibrary(
    state: TimelineUiState.Ready,
    callbacks: TimelineCallbacks,
) {
    val galleryState = rememberGalleryState()
    Column(modifier = Modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = stringResource(R.string.nav_photos),
            actions = {
                if (!state.geometry.isEmpty) {
                    IconButton(
                        onClick = { callbacks.onReviewSection(galleryState.firstVisibleSection) },
                        modifier = Modifier.capsuleTouchTarget(),
                    ) {
                        Icon(
                            imageVector = Icons.Filled.RateReview,
                            contentDescription = stringResource(R.string.timeline_action_a11y_enter_culling),
                        )
                    }
                }
            },
        )
        FailedActionBanner(outcome = state.pendingAction, onDismiss = callbacks.onOutcomeHandled)
        // The shell already says "you are offline" for the whole app. This says the one thing
        // the shell cannot: that this library has work outstanding which the missing
        // connection is holding up. Without that, a stalled upload marker looks like a fault.
        if (state.isOffline && state.hasPendingUploads) {
            CapsuleBanner(
                title = stringResource(R.string.timeline_offline_pending_title),
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .padding(StripInset)
                        .testTag(TimelineTestTags.OFFLINE_STRIP),
                body = stringResource(R.string.timeline_offline_body),
                severity = CapsuleBannerSeverity.Degraded,
                icon = Icons.Filled.CloudOff,
            )
        }
        Box(modifier = Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
            if (state.geometry.isEmpty) {
                TimelineEmptyState(reason = state.emptyReason, onImport = callbacks.onImport)
            } else {
                TimelineGrid(
                    state = state,
                    galleryState = galleryState,
                    callbacks = callbacks,
                    modifier = Modifier.fillMaxSize(),
                )
            }
        }
    }
}

/**
 * What happened to the last batch, when what happened was a refusal.
 *
 * Only failures are drawn. A batch that worked is evidenced by the grid it changed, and a
 * confirmation toast on every successful hide would be a toast the user learns to dismiss
 * without reading — which is exactly the habit that makes them miss this one. A success is
 * cleared as soon as it lands, so a later failure is never confused with an older win.
 *
 * The reason is the localized `error.*` sentence, never the code and never a transport status.
 */
@Composable
private fun FailedActionBanner(
    outcome: TimelineActionOutcome?,
    onDismiss: () -> Unit,
) {
    LaunchedEffect(outcome) { if (outcome != null && outcome.failure == null) onDismiss() }
    val failure = outcome?.failure ?: return
    CapsuleBanner(
        title = stringResource(R.string.timeline_action_failed_title),
        modifier =
            Modifier
                .fillMaxWidth()
                .padding(StripInset)
                .testTag(TimelineTestTags.ACTION_FAILURE),
        body = localizedError(failure),
        severity = CapsuleBannerSeverity.Error,
        action = {
            CapsuleButton(
                onClick = onDismiss,
                label = stringResource(R.string.common_close),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        },
    )
}

/**
 * Nothing to show, and why.
 *
 * Four bodies rather than one, because the remedy differs and because "no photos yet" said to
 * someone with three hundred photographs in their trash is a lie the app should not tell. The
 * import action is offered in every case: it is the one thing that works regardless of
 * whether there is a server, a connection, or an account.
 */
@Composable
private fun TimelineEmptyState(
    reason: TimelineEmptyReason?,
    onImport: () -> Unit,
) {
    val filtered = reason == TimelineEmptyReason.EverythingFiltered
    val title =
        if (filtered) {
            stringResource(R.string.timeline_empty_filtered_title)
        } else {
            stringResource(R.string.timeline_empty_never_imported_title)
        }
    val body =
        when (reason) {
            TimelineEmptyReason.EverythingFiltered -> stringResource(R.string.timeline_empty_filtered_body)
            TimelineEmptyReason.Offline -> stringResource(R.string.timeline_empty_offline_body)
            TimelineEmptyReason.NeverImportedLocalOnly -> stringResource(R.string.timeline_empty_local_only_body)
            TimelineEmptyReason.NeverImported, null ->
                stringResource(R.string.timeline_empty_never_imported_body)
        }
    CapsuleEmptyState(
        title = title,
        modifier = Modifier.fillMaxSize().testTag(TimelineTestTags.EMPTY),
        body = body,
        icon = Icons.Filled.PhotoLibrary,
        action = {
            CapsuleButton(
                onClick = onImport,
                label = stringResource(R.string.timeline_empty_never_imported_action),
            )
        },
    )
}
