package com.justin13888.capsule.core.designsystem.component

import androidx.compose.material3.ListItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow

/**
 * One row of a list.
 *
 * Takes its three text slots as strings rather than composables on purpose: a list row
 * whose headline is an arbitrary composable is a list row that will eventually contain a
 * nested button, and a row with two tap targets is a row a screen reader cannot describe.
 * Anything richer than "text, text, text with an icon on each end" wants [CapsuleCard].
 *
 * The parameter count is over the project's limit on purpose: three text slots, two
 * content slots, a click and a description is the minimum that covers the rows this app
 * actually draws, and folding them into a parameter object would move the same eight
 * decisions somewhere less discoverable.
 *
 * @param headline already-localized primary text.
 * @param supporting already-localized secondary text, on up to two lines.
 * @param overline already-localized text above the headline; for a category or a date.
 * @param contentDescription replaces the whole row's announcement when the concatenated
 *   text would read badly ("IMG_4821 · 2 · failed").
 */
@Suppress("LongParameterList")
@Composable
fun CapsuleListItem(
    headline: String,
    modifier: Modifier = Modifier,
    supporting: String? = null,
    overline: String? = null,
    leading: @Composable (() -> Unit)? = null,
    trailing: @Composable (() -> Unit)? = null,
    onClick: (() -> Unit)? = null,
    contentDescription: String? = null,
) {
    val rowModifier = modifier.capsuleTouchTarget().capsuleDescription(contentDescription)
    val overlineContent: (@Composable () -> Unit)? =
        overline?.let {
            { Text(text = it, maxLines = 1, overflow = TextOverflow.Ellipsis) }
        }
    val supportingContent: (@Composable () -> Unit)? =
        supporting?.let {
            { Text(text = it, maxLines = 2, overflow = TextOverflow.Ellipsis) }
        }
    val headlineContent: @Composable () -> Unit = {
        Text(text = headline, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
    if (onClick == null) {
        ListItem(
            modifier = rowModifier,
            leadingContent = leading,
            trailingContent = trailing,
            overlineContent = overlineContent,
            supportingContent = supportingContent,
            content = headlineContent,
        )
    } else {
        ListItem(
            onClick = onClick,
            modifier = rowModifier,
            leadingContent = leading,
            trailingContent = trailing,
            overlineContent = overlineContent,
            supportingContent = supportingContent,
            content = headlineContent,
        )
    }
}
