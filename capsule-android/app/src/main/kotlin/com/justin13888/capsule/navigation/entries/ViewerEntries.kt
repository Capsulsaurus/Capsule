package com.justin13888.capsule.navigation.entries

import androidx.compose.runtime.Composable
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.viewer.AssetInfoRoute
import com.justin13888.capsule.feature.viewer.MetadataEditorRoute
import com.justin13888.capsule.feature.viewer.ProvenanceRoute
import com.justin13888.capsule.feature.viewer.ViewerArgs
import com.justin13888.capsule.feature.viewer.ViewerControls
import com.justin13888.capsule.feature.viewer.ViewerNavigation
import com.justin13888.capsule.feature.viewer.ViewerStage
import com.justin13888.capsule.feature.viewer.rememberViewerState
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination
import com.justin13888.capsule.shell.panes.CapsuleSupportingPane
import com.justin13888.capsule.shell.panes.CapsuleTabletopSplit

/**
 * One asset, and everything the app can say about it.
 *
 * The viewer is where all three adaptive mechanisms meet, so the shell wires the layout and
 * leaves the feature module only the content:
 *
 * - it is immersive, so the navigation suite is gone at every width;
 * - the info panel is a supporting pane, so it sits beside the photo when there is room and
 *   becomes a separate screen when there is not;
 * - in tabletop posture the photo takes the upper half and the controls drop below the hinge.
 *
 * Filled in by `:capsule-android:feature:viewer`.
 */
fun EntryProviderScope<NavKey>.viewerEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.AssetViewer>(scope) { key -> ViewerPane(scope, key) }
    destination<CapsuleDestination.AssetInfo>(scope) { key -> AssetInfoRoute(assetId = key.assetId) }
    destination<CapsuleDestination.Provenance>(scope) { key -> ProvenanceRoute(assetId = key.assetId) }
    destination<CapsuleDestination.MetadataEdit>(scope) { key -> MetadataEditorRoute(assetId = key.assetId) }
}

/**
 * The viewer's adaptive frame: photo, controls, and the info panel beside or behind them.
 *
 * The view model is resolved once, here, and its state handed to both halves. That is the
 * whole reason the feature returns a presentation rather than two independent screens: the
 * photo and the strip can end up on opposite sides of a physical hinge, and two separately
 * resolved states would eventually disagree about which asset is open.
 */
@Composable
private fun ViewerPane(
    scope: CapsuleEntryScope,
    key: CapsuleDestination.AssetViewer,
) {
    val viewer =
        rememberViewerState(
            args = ViewerArgs(assetId = key.assetId, contextId = key.contextId),
            onReviewQuarantine = { scope.navigator.goTo(CapsuleDestination.Quarantine) },
            // The feature module cannot start a store listing, and the shell's own update
            // prompt only exists behind a protocol refusal. About is where this build's
            // version and protocol range are, which is what the user needs to see next.
            onUpdateApp = { scope.navigator.goTo(CapsuleDestination.SettingsAbout) },
        )
    CapsuleSupportingPane(
        mainPane = { showInfo ->
            CapsuleTabletopSplit(
                content = { ViewerStage(state = viewer.state, actions = viewer.actions, stage = viewer.stage) },
                controls = {
                    ViewerControls(
                        state = viewer.state,
                        actions = viewer.actions,
                        navigation = viewerNavigation(scope, key, showInfo),
                        stage = viewer.stage,
                    )
                },
            )
        },
        supportingPane = { AssetInfoRoute(assetId = key.assetId) },
    )
}

/** Where the viewer's chrome sends the user, in the shell's own destination vocabulary. */
private fun viewerNavigation(
    scope: CapsuleEntryScope,
    key: CapsuleDestination.AssetViewer,
    showInfo: () -> Unit,
): ViewerNavigation =
    ViewerNavigation(
        onOpenInfo = showInfo,
        onOpenProvenance = { scope.navigator.goTo(CapsuleDestination.Provenance(key.assetId)) },
        onOpenEditor = { scope.navigator.goTo(CapsuleDestination.MetadataEdit(key.assetId)) },
    )
