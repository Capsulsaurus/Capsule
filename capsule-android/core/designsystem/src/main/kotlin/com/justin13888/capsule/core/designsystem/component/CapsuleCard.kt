package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme

/** How thick a selected card's ring is. Thin enough not to crop the thumbnail inside it. */
private val SelectionRingWidth = 2.dp

/**
 * A container that groups content without shouting about it.
 *
 * Capsule cards are flat by default. Elevation in a photo library reads as a drop shadow
 * over an image, which competes with the image's own depth; separation here comes from
 * the container tone in the surface family instead.
 *
 * @param selected draws the selection ring from [CapsuleTheme]'s semantic colours. This
 *   is the one place a card is allowed an accent: selection is the state a photographer
 *   needs to read across a grid without looking directly at any one cell.
 * @param contentDescription announced label for the card as a whole. Supply it when the
 *   card's content is a thumbnail rather than text.
 */
@Composable
fun CapsuleCard(
    modifier: Modifier = Modifier,
    onClick: (() -> Unit)? = null,
    selected: Boolean = false,
    contentDescription: String? = null,
    content: @Composable ColumnScope.() -> Unit,
) {
    val border =
        if (selected) {
            BorderStroke(SelectionRingWidth, CapsuleTheme.colors.selected.color)
        } else {
            null
        }
    val cardModifier = modifier.capsuleDescription(contentDescription)
    val colors = CardDefaults.cardColors()
    if (onClick == null) {
        Card(modifier = cardModifier, colors = colors, border = border, content = content)
    } else {
        Card(
            onClick = onClick,
            modifier = cardModifier.capsuleTouchTarget(),
            colors = colors,
            border = border,
            content = content,
        )
    }
}
