package com.justin13888.capsule.feature.moderation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import org.koin.androidx.compose.koinViewModel
import org.koin.core.parameter.parametersOf

/**
 * The report destination, wired to its view model.
 *
 * @param subjectId whatever the caller was looking at — a content hash or an account. Ids
 *   travel as `String` because the shell's navigation keys do; the value classes are
 *   re-applied at the repository boundary.
 * @param onDone leaves once the report has landed.
 */
@Composable
fun ReportRoute(
    subjectId: String,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
    viewModel: ReportViewModel = koinViewModel(parameters = { parametersOf(subjectId) }),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    ReportScreen(
        state = state,
        actions =
            ReportActions(
                onKindChange = viewModel::onKindChange,
                onContentHashChange = viewModel::onContentHashChange,
                onPointerChange = viewModel::onPointerChange,
                onReasonChange = viewModel::onReasonChange,
                onAlsoBlockChange = viewModel::onAlsoBlockChange,
                onSubmit = viewModel::submit,
                onRetry = viewModel::retry,
            ),
        onDone = onDone,
        modifier = modifier,
    )
}

/** The blocked-list destination, wired to its view model. */
@Composable
fun BlocksRoute(
    modifier: Modifier = Modifier,
    viewModel: BlocksViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    BlocksScreen(
        state = state,
        onDraftChange = viewModel::onDraftChange,
        onBlock = viewModel::block,
        onUnblock = viewModel::unblock,
        onRetry = viewModel::reload,
        modifier = modifier,
    )
}

/**
 * The appeal destination, wired to its view model.
 *
 * @param caseId the record entry being contested, as the shell carried it.
 */
@Composable
fun AppealRoute(
    caseId: String,
    modifier: Modifier = Modifier,
    viewModel: AppealViewModel = koinViewModel(parameters = { parametersOf(caseId) }),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    AppealScreen(
        state = state,
        onStatementChange = viewModel::onStatementChange,
        onSubmit = viewModel::submit,
        onRetry = { if (state.phase == AppealPhase.Failed) viewModel.retry() else viewModel.reload() },
        modifier = modifier,
    )
}
