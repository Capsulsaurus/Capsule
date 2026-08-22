package com.justin13888.capsule.shell.panes

import androidx.compose.material3.adaptive.currentWindowAdaptiveInfoV2
import androidx.compose.material3.adaptive.layout.AnimatedPane
import androidx.compose.material3.adaptive.layout.ListDetailPaneScaffold
import androidx.compose.material3.adaptive.layout.ListDetailPaneScaffoldRole
import androidx.compose.material3.adaptive.layout.PaneScaffoldDirective
import androidx.compose.material3.adaptive.layout.SupportingPaneScaffold
import androidx.compose.material3.adaptive.layout.SupportingPaneScaffoldRole
import androidx.compose.material3.adaptive.layout.calculatePaneScaffoldDirectiveWithTwoPanesOnMediumWidth
import androidx.compose.material3.adaptive.navigation.rememberListDetailPaneScaffoldNavigator
import androidx.compose.material3.adaptive.navigation.rememberSupportingPaneScaffoldNavigator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.shell.FoldPosture
import com.justin13888.capsule.shell.LocalFoldPosture
import kotlinx.coroutines.launch

/**
 * The directive every Capsule pane scaffold is built from.
 *
 * Two departures from the Material default, both forced by what this app is for:
 *
 * - Two panes appear at *medium* width, not just expanded. An unfolded foldable is 600-840dp
 *   and is held like a small tablet; collapsing it to a single pane there would make the
 *   fold pointless, which is the whole reason `…WithTwoPanesOnMediumWidth` exists.
 * - In book posture the horizontal spacer is widened to the physical hinge. The default
 *   `HingePolicy.AvoidSeparating` already keeps content out of `excludedBounds`, but the
 *   spacer between the panes stays at its nominal 24dp, so the split and the hinge land in
 *   different places and the seam reads as a rendering bug. Matching them puts the pane
 *   boundary *on* the fold, which is where a two-page layout belongs.
 */
@Composable
fun capsulePaneDirective(foldPosture: FoldPosture = LocalFoldPosture.current): PaneScaffoldDirective {
    val adaptiveInfo = currentWindowAdaptiveInfoV2()
    val base = calculatePaneScaffoldDirectiveWithTwoPanesOnMediumWidth(adaptiveInfo)
    val hingeWidth = hingeSeparatorWidth(foldPosture)
    return remember(base, hingeWidth) {
        if (hingeWidth == null) base else base.copy(horizontalPartitionSpacerSize = hingeWidth)
    }
}

/** The hinge's on-screen width in dp when the device is in book posture, else null. */
@Composable
private fun hingeSeparatorWidth(foldPosture: FoldPosture): Dp? {
    val density = LocalDensity.current
    return remember(foldPosture, density) {
        val book = foldPosture as? FoldPosture.Book ?: return@remember null
        with(density) { book.hinge.width.toDp() }.takeIf { it > 0.dp }
    }
}

/**
 * A list beside its detail, collapsing to one pane at compact width.
 *
 * Offered by the shell rather than reimplemented per feature. Albums, people, share links,
 * sessions and the audit log are all the same shape — a listing whose rows open a detail —
 * and each one re-deriving "should the detail be a pane or a push" is how a supposedly
 * adaptive app ends up behaving differently on the same device depending which tab you are in.
 *
 * The scaffold owns its own back behaviour: at compact width the detail covers the list and
 * back returns to it, without ever touching the app's back stack. That separation is what
 * lets a feature adopt this without the shell knowing it did.
 *
 * @param listPane the listing. Receives a callback that opens the detail for a content key.
 * @param detailPane the detail. Receives the selected content key, or null before a selection.
 */
@Composable
fun CapsuleListDetailPane(
    listPane: @Composable (openDetail: (String) -> Unit) -> Unit,
    detailPane: @Composable (contentKey: String?) -> Unit,
    modifier: Modifier = Modifier,
) {
    val directive = capsulePaneDirective()
    val navigator = rememberListDetailPaneScaffoldNavigator<String>(scaffoldDirective = directive)
    val scope = rememberCoroutineScope()

    ListDetailPaneScaffold(
        directive = navigator.scaffoldDirective,
        value = navigator.scaffoldValue,
        listPane = {
            AnimatedPane {
                listPane { key ->
                    scope.launch {
                        navigator.navigateTo(ListDetailPaneScaffoldRole.Detail, key)
                    }
                }
            }
        },
        detailPane = {
            AnimatedPane {
                detailPane(navigator.currentDestination?.contentKey)
            }
        },
        modifier = modifier,
    )
}

/**
 * A main surface with an information panel that becomes a side pane when there is room.
 *
 * This is the viewer's shape: the photo is the main pane and its metadata is the supporting
 * one. On a phone the panel is a separate screen the user opens; on a tablet it sits beside
 * the photo, because a professional checking exposure against ISO should not have to leave
 * the photo to read the number.
 *
 * @param mainPane the primary surface. Receives a callback that reveals the supporting pane.
 * @param supportingPane the information panel.
 */
@Composable
fun CapsuleSupportingPane(
    mainPane: @Composable (showSupporting: () -> Unit) -> Unit,
    supportingPane: @Composable () -> Unit,
    modifier: Modifier = Modifier,
) {
    val directive = capsulePaneDirective()
    val navigator = rememberSupportingPaneScaffoldNavigator<String>(scaffoldDirective = directive)
    val scope = rememberCoroutineScope()

    SupportingPaneScaffold(
        directive = navigator.scaffoldDirective,
        value = navigator.scaffoldValue,
        mainPane = {
            AnimatedPane {
                mainPane {
                    scope.launch {
                        navigator.navigateTo(SupportingPaneScaffoldRole.Supporting)
                    }
                }
            }
        },
        supportingPane = { AnimatedPane { supportingPane() } },
        modifier = modifier,
    )
}
