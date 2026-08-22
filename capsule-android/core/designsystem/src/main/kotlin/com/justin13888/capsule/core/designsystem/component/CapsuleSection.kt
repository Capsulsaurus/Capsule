package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

private val SectionHeaderPadding = 16.dp
private val SectionHeaderTopPadding = 24.dp

/** The inset that lines a divider up with text that follows a 40dp leading icon. */
private val DividerInset = 72.dp

/**
 * The label above a group of rows.
 *
 * Uses the emphasized title style rather than a coloured one. Colour is the app's
 * scarcest signal — it is spoken for by selection and sync state — so hierarchy in the
 * chrome is carried by weight and space instead.
 *
 * @param title already-localized text.
 * @param trailing a slot for a count or a "see all" action.
 */
@Composable
fun CapsuleSectionHeader(
    title: String,
    modifier: Modifier = Modifier,
    trailing: (@Composable () -> Unit)? = null,
) {
    Row(
        modifier =
            modifier.padding(
                start = SectionHeaderPadding,
                end = SectionHeaderPadding,
                top = SectionHeaderTopPadding,
                bottom = SectionHeaderPadding / 2,
            ),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title,
            style = MaterialTheme.typography.titleSmallEmphasized,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = false),
        )
        trailing?.invoke()
    }
}

/**
 * A hairline between rows.
 *
 * @param inset aligns the rule with the text of a row that has a leading icon, so a list
 *   reads as one column rather than two. Off by default: a full-bleed rule is correct
 *   between sections, and an inset one between peers.
 */
@Composable
fun CapsuleDivider(
    modifier: Modifier = Modifier,
    inset: Boolean = false,
) {
    HorizontalDivider(
        modifier = if (inset) modifier.padding(start = DividerInset) else modifier,
        color = MaterialTheme.colorScheme.outlineVariant,
    )
}
