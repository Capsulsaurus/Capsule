package com.justin13888.capsule.feature.people

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import org.koin.androidx.compose.koinViewModel
import org.koin.core.parameter.parametersOf

/**
 * The People hub, wired to its view model.
 *
 * One of the two composables the app module calls. Everything below it is stateless, which is
 * what lets each of the hub's four states be rendered from a preview or a test without a
 * graph; the route exists only to bind the view model and to turn navigation into the plain
 * `(String) -> Unit` callbacks the screen declares.
 */
@Composable
fun PeopleRoute(
    onOpenPerson: (String) -> Unit,
    onOpenAsset: (String) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: PeopleViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions =
        remember(viewModel, onOpenPerson, onOpenAsset) {
            PeopleActions(
                onOpenPerson = onOpenPerson,
                onOpenAsset = onOpenAsset,
                onNameRequested = viewModel::onNameRequested,
                onNameSubmit = viewModel::onNameSubmit,
                onAttachCluster = viewModel::onAttachCluster,
                onMergeOpen = viewModel::onMergeOpen,
                onMergeInto = viewModel::onMergeInto,
                onDismissSheets = viewModel::onDismissSheets,
                onHide = viewModel::onHide,
                onUndoHide = viewModel::onUndoHide,
                onOpenSmartAlbum = viewModel::onOpenSmartAlbum,
                onCloseSmartAlbum = viewModel::onCloseSmartAlbum,
                onDismissActionFailure = viewModel::onDismissActionFailure,
                onRetry = viewModel::onRetry,
            )
        }
    PeopleScreen(state = state, actions = actions, modifier = modifier)
}

/**
 * One person's screen, wired to its view model.
 *
 * The id arrives as a `String` because the shell's navigation keys carry it that way, and is
 * re-wrapped into its value class at the repository boundary inside the view model. Koin is
 * handed it as a parameter rather than the screen reading a saved-state handle, so the view
 * model stays constructible from a test with one argument.
 */
@Composable
fun PersonDetailRoute(
    personId: String,
    onBack: () -> Unit,
    onOpenAsset: (String) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: PersonDetailViewModel = koinViewModel { parametersOf(personId) },
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions =
        remember(viewModel, onBack, onOpenAsset) {
            PersonDetailActions(
                onBack = onBack,
                onOpenAsset = onOpenAsset,
                onRenameRequested = viewModel::onRenameRequested,
                onRenameSubmit = viewModel::onRenameSubmit,
                onPromote = viewModel::onPromote,
                onDismissSuggestion = viewModel::onDismissSuggestion,
                onDismissActionFailure = viewModel::onDismissActionFailure,
                onRetry = viewModel::onRetry,
            )
        }
    PersonDetailScreen(state = state, actions = actions, modifier = modifier)
}
