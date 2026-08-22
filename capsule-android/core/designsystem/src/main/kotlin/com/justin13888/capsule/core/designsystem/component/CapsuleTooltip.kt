package com.justin13888.capsule.core.designsystem.component

import androidx.compose.material3.PlainTooltip
import androidx.compose.material3.Text
import androidx.compose.material3.TooltipAnchorPosition
import androidx.compose.material3.TooltipBox
import androidx.compose.material3.TooltipDefaults
import androidx.compose.material3.rememberTooltipState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * Names an icon-only control on long press and on keyboard focus.
 *
 * The floating toolbars in this app are almost entirely icons — there is no room for
 * labels over a photograph — which makes tooltips the only way a user discovers what the
 * icons do without tapping them. Wrapping every icon control in one is the price of an
 * icon-only chrome.
 *
 * @param text already-localized text. This is the label a caller would otherwise pass as
 *   a content description, and it should say the same thing.
 */
@Composable
fun CapsuleTooltip(
    text: String,
    modifier: Modifier = Modifier,
    anchorPosition: TooltipAnchorPosition = TooltipAnchorPosition.Above,
    content: @Composable () -> Unit,
) {
    TooltipBox(
        positionProvider = TooltipDefaults.rememberTooltipPositionProvider(anchorPosition),
        tooltip = { PlainTooltip { Text(text = text) } },
        state = rememberTooltipState(),
        modifier = modifier,
        content = content,
    )
}
