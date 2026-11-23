package com.justin13888.capsule.feature.search

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCheckbox
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleSheet
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.ImmutableList

/** Test handle for the advanced filter sheet. */
const val SEARCH_FILTER_SHEET_TAG: String = "search.filterSheet"

private val SheetPadding = 16.dp
private val SheetSpacing = 8.dp
private val SheetMaxHeight = 520.dp

/**
 * Every filter, not just the ones that fit in the chip row.
 *
 * A sheet rather than a screen because the answer stays behind it: the user is narrowing
 * results they can still see, and pushing a destination would make each adjustment a
 * round trip through the back stack.
 *
 * Values are checkboxes rather than chips here. The chip row is for the two or three facets
 * a user reaches for constantly; this list is where they go to find the ninth camera, and a
 * checkbox column is the only shape that stays readable at that length and at 200% font
 * scale.
 */
@Composable
fun SearchFilterSheet(
    groups: ImmutableList<FacetGroup>,
    filters: SearchFilters,
    onToggleFacet: (FacetRef) -> Unit,
    onClearFilters: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleSheet(onDismissRequest = onDismiss, modifier = modifier.testTag(SEARCH_FILTER_SHEET_TAG)) {
        CapsuleSectionHeader(title = stringResource(R.string.search_filters_title))
        Column(
            modifier =
                Modifier
                    .fillMaxWidth()
                    .heightIn(max = SheetMaxHeight)
                    .verticalScroll(rememberScrollState()),
        ) {
            groups.forEach { group ->
                CapsuleSectionHeader(title = facetKindLabel(group.kind))
                group.values.forEach { value ->
                    FacetCheckbox(kind = group.kind, value = value, filters = filters, onToggleFacet = onToggleFacet)
                }
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth().padding(SheetPadding),
            horizontalArrangement = Arrangement.spacedBy(SheetSpacing, Alignment.End),
        ) {
            CapsuleButton(
                onClick = onClearFilters,
                label = stringResource(R.string.search_filters_clear),
                emphasis = CapsuleButtonEmphasis.Tertiary,
                enabled = !filters.isEmpty,
            )
            CapsuleButton(
                onClick = onDismiss,
                label = stringResource(R.string.search_filters_apply),
            )
        }
    }
}

/**
 * One value, with its share of the hits beside it.
 *
 * A row that owns the whole tap target, with the checkbox drawn as state rather than as a
 * second control: a list row with two independently focusable targets is a row a screen
 * reader cannot describe, and at these list lengths it is also a mis-tap generator.
 */
@Composable
private fun FacetCheckbox(
    kind: FacetKind,
    value: FacetValue,
    filters: SearchFilters,
    onToggleFacet: (FacetRef) -> Unit,
) {
    val kindLabel = facetKindLabel(kind)
    val valueLabel = facetValueLabel(value.point.label)
    val selected = value.ref in filters
    CapsuleListItem(
        headline = valueLabel,
        modifier = Modifier.fillMaxWidth(),
        supporting = value.count.toString(),
        trailing = { CapsuleCheckbox(checked = selected, onCheckedChange = null) },
        onClick = { onToggleFacet(value.ref) },
        contentDescription =
            stringResource(R.string.search_a11y_facet)
                .withArguments("facet" to kindLabel, "value" to valueLabel, "count" to value.count),
    )
}
