package com.justin13888.capsule.feature.search

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.FilterList
import androidx.compose.material.icons.filled.History
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleChip
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/** Test handle for the query box. */
const val SEARCH_FIELD_TAG: String = "search.field"

/** Test handle for the stale-model notice, which must be findable regardless of its wording. */
const val SEARCH_STALE_NOTICE_TAG: String = "search.staleNotice"

/** Test handle for the button that opens the advanced filter sheet. */
const val SEARCH_ADVANCED_TAG: String = "search.advanced"

private val ScreenPadding = 16.dp
private val ScreenSpacing = 12.dp

/**
 * Everything the search screen can ask for, in one object.
 *
 * Grouped so the screen stays a plain function of state plus intent: nine separate callback
 * parameters would push the composable past the project's parameter limit and would make
 * every preview and every test restate all nine.
 */
@Suppress("LongParameterList")
@Immutable
class SearchActions(
    /** The query box changed. */
    val onQueryChange: (String) -> Unit,
    /** The user asked for what is in the box. */
    val onSubmit: () -> Unit,
    /** A previous query was picked. */
    val onRecentSelected: (String) -> Unit,
    /** The recent-search list was emptied. */
    val onClearRecents: () -> Unit,
    /** A facet chip was flipped. */
    val onToggleFacet: (FacetRef) -> Unit,
    /** Every filter off. */
    val onClearFilters: () -> Unit,
    /** The advanced filter sheet was opened or dismissed. */
    val onFilterSheetOpenChange: (Boolean) -> Unit,
    /** The remediation on the error state. */
    val onRetry: () -> Unit,
    /** A result was opened. */
    val onOpenAsset: (String) -> Unit,
) {
    companion object {
        /** A set that does nothing, for previews and for state-only tests. */
        val Inert: SearchActions =
            SearchActions({}, {}, {}, {}, {}, {}, {}, {}, {})
    }
}

/**
 * Search: the box, the facets, and whichever of the five states currently applies.
 *
 * Stateless by construction — it takes a value and a bag of callbacks — so every state it
 * can be in is one line to reach from a preview or a test, including the two that are
 * otherwise awkward to produce: a failure, and a library whose AI outputs are being held
 * back because a model version changed.
 */
@Composable
fun SearchScreen(
    state: SearchUiState,
    actions: SearchActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(title = stringResource(R.string.nav_search))
        SearchField(query = state.query, actions = actions)
        state.staleModels?.let { notice ->
            StaleModelNoticeBanner(
                notice = notice,
                modifier = Modifier.padding(horizontal = ScreenPadding, vertical = ScreenSpacing / 2),
            )
        }
        if (state.phase != SearchPhase.Entry && state.groups.isNotEmpty()) {
            FilterBar(state = state, actions = actions)
        }
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            when (state.phase) {
                SearchPhase.Loading ->
                    CapsuleLoadingState(
                        modifier = Modifier.fillMaxSize(),
                        title = stringResource(R.string.search_loading),
                        contentDescription = stringResource(R.string.search_loading),
                    )

                SearchPhase.Entry -> EntryContent(state = state, actions = actions)
                SearchPhase.Results -> ResultsContent(state = state, actions = actions)
                SearchPhase.NoResults -> NoResultsContent(state = state, actions = actions)
                SearchPhase.Failed -> FailureContent(failure = state.failure, onRetry = actions.onRetry)
            }
        }
    }
    if (state.filterSheetOpen) {
        SearchFilterSheet(
            groups = state.groups,
            filters = state.filters,
            onToggleFacet = actions.onToggleFacet,
            onClearFilters = actions.onClearFilters,
            onDismiss = { actions.onFilterSheetOpenChange(false) },
        )
    }
}

/**
 * The query box.
 *
 * A raw Material text field, and the one place this module reaches past the design system:
 * `core:designsystem` has no text-field wrapper yet, and inventing a local one here would
 * put a second answer to "what does a Capsule input look like" in a feature module.
 */
@Composable
private fun SearchField(
    query: String,
    actions: SearchActions,
) {
    OutlinedTextField(
        value = query,
        onValueChange = actions.onQueryChange,
        modifier =
            Modifier
                .fillMaxWidth()
                .padding(horizontal = ScreenPadding)
                .testTag(SEARCH_FIELD_TAG),
        label = { Text(text = stringResource(R.string.search_field_label)) },
        placeholder = { Text(text = stringResource(R.string.nav_search_placeholder)) },
        leadingIcon = { Icon(imageVector = Icons.Filled.Search, contentDescription = null) },
        trailingIcon = {
            if (query.isNotEmpty()) {
                IconButton(onClick = { actions.onQueryChange("") }) {
                    Icon(
                        imageVector = Icons.Filled.Close,
                        contentDescription = stringResource(R.string.search_field_clear),
                    )
                }
            }
        },
        singleLine = true,
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
        keyboardActions = KeyboardActions(onSearch = { actions.onSubmit() }),
    )
}

/**
 * The held-back-AI notice.
 *
 * Degraded rather than an error, and permanent rather than a snackbar: excluded outputs are
 * a condition that lasts until regeneration finishes, and a user who saw it flash past would
 * only learn that their library had quietly shrunk. It carries no action because there is
 * nothing here for the user to press — regeneration is a background walk of the library, not
 * a decision.
 */
@Composable
private fun StaleModelNoticeBanner(
    notice: StaleModelNotice,
    modifier: Modifier = Modifier,
) {
    val body = stringResource(R.string.search_stale_model_body).withArguments("count" to notice.excludedCount)
    val slots = stringResource(R.string.search_stale_model_slots).withArguments("slots" to notice.slots.joinToString())
    CapsuleBanner(
        title = stringResource(R.string.search_stale_model_title),
        modifier = modifier.fillMaxWidth().testTag(SEARCH_STALE_NOTICE_TAG),
        body = "$body $slots",
        severity = CapsuleBannerSeverity.Degraded,
    )
}

/** The chip row over the results, plus the door to every other filter. */
@Composable
private fun FilterBar(
    state: SearchUiState,
    actions: SearchActions,
) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(ScreenSpacing / 2),
    ) {
        FacetChipRow(
            values = state.groups.flatMap { group -> group.values.take(CHIPS_PER_KIND) },
            filters = state.filters,
            onToggleFacet = actions.onToggleFacet,
        )
        val activeLabel =
            if (state.filters.isEmpty) {
                stringResource(R.string.search_filters_advanced)
            } else {
                stringResource(R.string.search_filters_advanced) + " · " +
                    stringResource(R.string.search_filters_active).withArguments("count" to state.filters.count)
            }
        CapsuleButton(
            onClick = { actions.onFilterSheetOpenChange(true) },
            label = activeLabel,
            modifier = Modifier.testTag(SEARCH_ADVANCED_TAG),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            icon = Icons.Filled.FilterList,
        )
    }
}

/** Nothing asked yet: what was asked before, and where to start from. */
@Composable
private fun EntryContent(
    state: SearchUiState,
    actions: SearchActions,
) {
    if (!state.hasEntryContent) {
        CapsuleEmptyState(
            title = stringResource(R.string.search_empty_title),
            modifier = Modifier.fillMaxSize(),
            body = stringResource(R.string.search_empty_body),
            icon = Icons.Filled.Search,
        )
        return
    }
    Column(
        modifier =
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(ScreenSpacing / 2),
    ) {
        if (state.recents.isNotEmpty()) {
            CapsuleSectionHeader(
                title = stringResource(R.string.search_recents_title),
                trailing = {
                    CapsuleButton(
                        onClick = actions.onClearRecents,
                        label = stringResource(R.string.search_recents_clear),
                        emphasis = CapsuleButtonEmphasis.Tertiary,
                    )
                },
            )
            state.recents.forEach { recent ->
                CapsuleListItem(
                    headline = recent,
                    leading = { Icon(imageVector = Icons.Filled.History, contentDescription = null) },
                    onClick = { actions.onRecentSelected(recent) },
                )
            }
        }
        if (state.suggested.isNotEmpty()) {
            CapsuleSectionHeader(title = stringResource(R.string.search_suggested_title))
            FacetChipRow(
                values = state.suggested,
                filters = state.filters,
                onToggleFacet = actions.onToggleFacet,
            )
        }
        if (state.completions.isNotEmpty()) {
            CapsuleSectionHeader(title = stringResource(R.string.search_suggestions_title))
            state.completions.forEach { completion ->
                CapsuleListItem(headline = completion, onClick = { actions.onRecentSelected(completion) })
            }
        }
    }
}

/** The answer. */
@Composable
private fun ResultsContent(
    state: SearchUiState,
    actions: SearchActions,
) {
    Column(modifier = Modifier.fillMaxSize()) {
        Text(
            text =
                stringResource(R.string.search_results_count)
                    .withArguments("count" to state.results.size, "total" to state.hitCount),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = ScreenPadding, vertical = ScreenSpacing / 2),
        )
        SearchResultsGrid(
            results = state.results,
            onOpenAsset = actions.onOpenAsset,
            modifier = Modifier.weight(1f).fillMaxWidth(),
        )
    }
}

/**
 * The query ran and nothing survived it.
 *
 * Separate from the empty state, and with a different exit: the usual cause is a filter the
 * user forgot was on, so the remediation offered is turning them off rather than typing
 * something else.
 */
@Composable
private fun NoResultsContent(
    state: SearchUiState,
    actions: SearchActions,
) {
    CapsuleEmptyState(
        title = stringResource(R.string.search_no_results_title),
        modifier = Modifier.fillMaxSize(),
        body = stringResource(R.string.search_no_results_body).withArguments("query" to state.query),
        icon = Icons.Filled.Search,
        action =
            if (state.filters.isEmpty) {
                null
            } else {
                {
                    CapsuleButton(
                        onClick = actions.onClearFilters,
                        label = stringResource(R.string.search_no_results_clear_filters),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                }
            },
    )
}

/** The query could not run. Always with a way back. */
@Composable
private fun FailureContent(
    failure: SearchFailure?,
    onRetry: () -> Unit,
) {
    val resolved = failure ?: SearchFailure.Unknown
    CapsuleErrorState(
        title = failureTitle(resolved),
        modifier = Modifier.fillMaxSize(),
        body = failureBody(resolved),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.search_error_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

/**
 * A scrolling row of facet chips.
 *
 * Scrolls rather than wraps, for the same reason the design system's own chip row does: a
 * wrapping row reflows the grid beneath it every time a filter is flipped, and a moving grid
 * is the one thing a photographer scanning thumbnails cannot work with.
 */
@Composable
private fun FacetChipRow(
    values: List<FacetValue>,
    filters: SearchFilters,
    onToggleFacet: (FacetRef) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(ScreenSpacing / 2),
    ) {
        values.forEach { value ->
            val kind = facetKindLabel(value.ref.kind)
            val label = facetValueLabel(value.point.label)
            CapsuleChip(
                label = label,
                selected = value.ref in filters,
                onSelectedChange = { onToggleFacet(value.ref) },
                contentDescription =
                    stringResource(R.string.search_a11y_facet)
                        .withArguments("facet" to kind, "value" to label, "count" to value.count),
            )
        }
    }
}

/** How many values of one dimension reach the chip row before the sheet takes over. */
private const val CHIPS_PER_KIND = 3
