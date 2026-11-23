package com.justin13888.capsule.feature.recovery

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import org.koin.androidx.compose.koinViewModel

/**
 * The recovery hub, wired to its view model.
 *
 * @param onVerify opens the local check.
 * @param onRewrap opens the guided re-wrap.
 * @param onShamir opens guardian-share setup.
 * @param onExport opens backup export.
 * @param onRestore opens backup restore.
 */
@Composable
fun RecoveryStatusRoute(
    onVerify: () -> Unit,
    onRewrap: () -> Unit,
    onShamir: () -> Unit,
    onExport: () -> Unit,
    onRestore: () -> Unit,
    modifier: Modifier = Modifier,
    viewModel: RecoveryStatusViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions =
        remember(viewModel, onVerify, onRewrap, onShamir, onExport, onRestore) {
            RecoveryStatusActions(
                onVerify = onVerify,
                onSnoozeDay = { viewModel.snooze(RecoveryStatusViewModel.SNOOZE_DAY) },
                onSnoozeWeek = { viewModel.snooze(RecoveryStatusViewModel.SNOOZE_WEEK) },
                onRewrap = onRewrap,
                onShamir = onShamir,
                onExport = onExport,
                onRestore = onRestore,
            )
        }
    RecoveryStatusScreen(state = state, actions = actions, onRetry = viewModel::reload, modifier = modifier)
}

/**
 * The local verification, wired to its view model.
 *
 * @param onRewrap opens the guided re-wrap, offered once attempts keep missing.
 */
@Composable
fun RecoveryVerifyRoute(
    onRewrap: () -> Unit,
    modifier: Modifier = Modifier,
    viewModel: RecoveryVerifyViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    RecoveryVerifyScreen(
        state = state,
        onSecretChange = viewModel::onSecretChange,
        onRevealChange = viewModel::onRevealChange,
        onVerify = viewModel::verify,
        onReset = viewModel::reset,
        onRewrap = onRewrap,
        modifier = modifier,
    )
}

/**
 * The guided re-wrap, wired to its view model.
 *
 * @param onReissueShares opens guardian-share setup once old shares are invalidated.
 */
@Composable
fun RecoveryRewrapRoute(
    onReissueShares: () -> Unit,
    modifier: Modifier = Modifier,
    viewModel: RecoveryRewrapViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    RecoveryRewrapScreen(
        state = state,
        onBegin = viewModel::begin,
        onWrittenDown = viewModel::onSecretWrittenDown,
        onTypedChange = viewModel::onTypedChange,
        onConfirm = viewModel::confirm,
        onAfter = RecoveryRewrapEndings(onReissueShares = onReissueShares, onReset = viewModel::reset),
        modifier = modifier,
    )
}

/** Guardian-share setup, wired to its view model. */
@Composable
fun ShamirRoute(
    modifier: Modifier = Modifier,
    viewModel: ShamirViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions =
        remember(viewModel) {
            ShamirActions(
                onThresholdChange = viewModel::onThresholdChange,
                onTotalChange = viewModel::onTotalChange,
                onGenerate = viewModel::generate,
                onEditHolder = viewModel::onEditHolder,
                onHolderDraftChange = viewModel::onHolderDraftChange,
                onMarkDelivered = viewModel::markDelivered,
            )
        }
    ShamirScreen(state = state, actions = actions, modifier = modifier)
}

/** Backup export, wired to its view model. */
@Composable
fun BackupExportRoute(
    modifier: Modifier = Modifier,
    viewModel: BackupExportViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    BackupExportScreen(
        state = state,
        onDestinationChange = viewModel::onDestinationChange,
        onExport = viewModel::export,
        onReset = viewModel::reset,
        onRetry = viewModel::reload,
        modifier = modifier,
    )
}

/** Backup restore, wired to its view model. */
@Composable
fun BackupRestoreRoute(
    modifier: Modifier = Modifier,
    viewModel: BackupRestoreViewModel = koinViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions =
        remember(viewModel) {
            BackupRestoreActions(
                onPathChange = viewModel::onPathChange,
                onPreview = viewModel::preview,
                onDryRun = viewModel::dryRun,
                onBeginCommit = viewModel::beginCommit,
                onConfirmationChange = viewModel::onConfirmationChange,
                onCommit = viewModel::commit,
                onReviewConflicts = viewModel::onReviewConflicts,
            )
        }
    BackupRestoreScreen(
        state = state,
        actions = actions,
        onDecision = viewModel::onDecision,
        onApplyMerge = viewModel::applyMerge,
        onReset = viewModel::reset,
        modifier = modifier,
    )
}
