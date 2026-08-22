package com.justin13888.capsule.navigation.entries

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.VerifiedUser
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination
import com.justin13888.capsule.placeholder.PlaceholderScreen
import com.justin13888.capsule.shell.panes.CapsuleSupportingPane
import com.justin13888.capsule.shell.panes.CapsuleTabletopSplit

/** Test handle for the viewer's control strip, which moves below the hinge in tabletop. */
const val VIEWER_CONTROLS_TAG: String = "shell.viewer-controls"

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
    destination<CapsuleDestination.AssetInfo>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Provenance>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.MetadataEdit>(scope) { scope.Placeholder(it) }
}

/** The viewer's adaptive frame: photo, controls, and the info panel beside or behind them. */
@Composable
private fun ViewerPane(
    scope: CapsuleEntryScope,
    key: CapsuleDestination.AssetViewer,
) {
    CapsuleSupportingPane(
        mainPane = { showInfo ->
            CapsuleTabletopSplit(
                content = { PlaceholderScreen(destination = key, navigator = scope.navigator) },
                controls = { ViewerControls(scope, key, showInfo) },
            )
        },
        supportingPane = {
            PlaceholderScreen(
                destination = CapsuleDestination.AssetInfo(key.assetId),
                navigator = scope.navigator,
            )
        },
    )
}

/**
 * The strip the user operates the viewer with.
 *
 * Separated from the photo rather than overlaid on it because that is what makes the tabletop
 * arrangement possible at all: an overlay has nowhere to go when the window splits in half.
 */
@Composable
private fun ViewerControls(
    scope: CapsuleEntryScope,
    key: CapsuleDestination.AssetViewer,
    onShowInfo: () -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(ControlPadding).testTag(VIEWER_CONTROLS_TAG),
        horizontalArrangement = Arrangement.spacedBy(ControlSpacing),
    ) {
        CapsuleButton(
            onClick = onShowInfo,
            label = stringResource(CapsuleDestination.AssetInfo(key.assetId).titleRes),
            emphasis = CapsuleButtonEmphasis.Secondary,
            icon = Icons.Filled.Info,
        )
        CapsuleButton(
            onClick = { scope.navigator.goTo(CapsuleDestination.Provenance(key.assetId)) },
            label = stringResource(CapsuleDestination.Provenance(key.assetId).titleRes),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            icon = Icons.Filled.VerifiedUser,
        )
    }
}

private val ControlPadding = 16.dp
private val ControlSpacing = 8.dp
