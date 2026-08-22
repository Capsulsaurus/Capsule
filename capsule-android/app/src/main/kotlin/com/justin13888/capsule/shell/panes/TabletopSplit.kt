package com.justin13888.capsule.shell.panes

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.shell.FoldPosture
import com.justin13888.capsule.shell.LocalFoldPosture

/** Test handle for the tabletop arrangement, so a test can tell the two branches apart. */
const val TABLETOP_SPLIT_TAG: String = "shell.tabletop-split"

/**
 * Content above the hinge, controls below it, when the device is stood up on a table.
 *
 * This is the one fold posture that changes what a screen *means* rather than how much of it
 * fits. A half-opened horizontal hinge turns the device into a tiny laptop: the upper half is
 * a screen the user is looking at from across a table, the lower half is the part their hands
 * are resting on. Putting a photo across both halves means the crease runs through the
 * subject; putting the controls in the upper half means reaching over the fold to press them.
 *
 * Off tabletop this degrades to the obvious thing — content takes the space, controls sit
 * under it — so a caller uses it unconditionally and never branches on posture itself.
 *
 * @param content the thing being looked at. Gets the space above the hinge.
 * @param controls the thing being operated. Gets the space below it.
 */
@Composable
fun CapsuleTabletopSplit(
    content: @Composable () -> Unit,
    controls: @Composable ColumnScope.() -> Unit,
    modifier: Modifier = Modifier,
    foldPosture: FoldPosture = LocalFoldPosture.current,
) {
    val split = tabletopSplit(foldPosture)
    Column(modifier = modifier.fillMaxSize().testTag(TABLETOP_SPLIT_TAG)) {
        if (split == null) {
            Box(modifier = Modifier.fillMaxWidth().weight(1f)) { content() }
        } else {
            Box(modifier = Modifier.fillMaxWidth().height(split.contentHeight)) { content() }
            // The hinge itself: left empty rather than padded into a neighbour, so nothing is
            // ever drawn on a crease the user cannot see through.
            Box(modifier = Modifier.fillMaxWidth().height(split.hingeHeight))
        }
        controls()
    }
}

/** The measured halves of a tabletop window, in dp. */
private data class TabletopMetrics(
    val contentHeight: Dp,
    val hingeHeight: Dp,
)

/** Measures the tabletop halves, or returns null when the device is not in tabletop. */
@Composable
private fun tabletopSplit(foldPosture: FoldPosture): TabletopMetrics? {
    val density = LocalDensity.current
    return remember(foldPosture, density) {
        val tabletop = foldPosture as? FoldPosture.Tabletop ?: return@remember null
        with(density) {
            val above = tabletop.hinge.top.toDp()
            // A hinge reported at the very top is a posture we cannot honour — there is no
            // upper half to put content in — so fall back to the ordinary arrangement.
            if (above <= 0.dp) null else TabletopMetrics(above, tabletop.hinge.height.toDp())
        }
    }
}
