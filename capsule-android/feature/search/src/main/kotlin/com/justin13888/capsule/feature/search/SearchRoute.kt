package com.justin13888.capsule.feature.search

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import org.koin.androidx.compose.koinViewModel

/**
 * The search destination, wired to its view model.
 *
 * The one composable the app module calls. Everything below it is stateless, which is what
 * lets the five states this screen can be in be rendered from a preview or a test without a
 * graph — the route exists only to bind the view model and to turn navigation into the
 * plain `(String) -> Unit` callback the screen declares.
 *
 * @param onOpenAsset opens one result. Ids travel as `String` because the shell's navigation
 *   keys do; the value class is re-applied at the repository boundary.
 */
@Composable
fun SearchRoute(
    onOpenAsset: (String) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: SearchViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions =
        remember(viewModel, onOpenAsset) {
            SearchActions(
                onQueryChange = viewModel::onQueryChange,
                onSubmit = viewModel::onSubmit,
                onRecentSelected = viewModel::onRecentSelected,
                onClearRecents = viewModel::onClearRecents,
                onToggleFacet = viewModel::onToggleFacet,
                onClearFilters = viewModel::onClearFilters,
                onFilterSheetOpenChange = viewModel::onFilterSheetOpenChange,
                onRetry = viewModel::onRetry,
                onOpenAsset = onOpenAsset,
            )
        }
    SearchScreen(state = state, actions = actions, modifier = modifier)
}
