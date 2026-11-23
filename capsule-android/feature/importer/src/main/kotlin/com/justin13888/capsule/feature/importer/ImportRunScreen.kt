package com.justin13888.capsule.feature.importer

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleProgress
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ImportFileState

/** Test handle for the per-file list. */
const val IMPORT_RUN_FILES_TAG: String = "importer.run-files"

/** Test handle for the pause-on-disconnect notice. */
const val IMPORT_PAUSED_TAG: String = "importer.paused"

/** Test handle for the sliding-window notice. */
const val IMPORT_WINDOW_TAG: String = "importer.window"

/** Test handle for the import summary. */
const val IMPORT_SUMMARY_TAG: String = "importer.summary"

/** Every callback the execution screen raises. */
data class ImportRunActions(
    /** Stops the run where it stands. */
    val onCancel: () -> Unit,
    /** Re-attaches to the run after a local failure. */
    val onRetry: () -> Unit,
    /** Leaves for the library once the run has finished. */
    val onOpenLibrary: () -> Unit,
    /** Leaves the screen. */
    val onBack: () -> Unit,
)

/**
 * A running import, file by file.
 *
 * Every admitted file is listed with the rung it is on, because "importing 41 photos" says
 * nothing about whether anything is moving. Under streaming the list also states the window —
 * how many files are on the device at once — since that bound is the whole reason a run
 * larger than the free space can finish at all.
 */
@Composable
fun ImportRunScreen(
    state: ImportRunUiState,
    actions: ImportRunActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_import_progress),
            navigationIcon = {
                IconButton(onClick = actions.onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.android_shell_back),
                    )
                }
            },
        )
        when {
            state.isLoading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxSize(),
                    title = stringResource(R.string.android_importer_plan_loading),
                    contentDescription = stringResource(R.string.android_importer_plan_loading),
                )

            state.hasFailed ->
                CapsuleErrorState(
                    title = stringResource(R.string.android_importer_run_failure_title),
                    modifier = Modifier.fillMaxSize(),
                    body = localizedErrorMessage(state.failure),
                    action = {
                        CapsuleButton(
                            onClick = actions.onRetry,
                            label = stringResource(R.string.android_importer_retry),
                        )
                    },
                )

            state.showsSummary -> ImportSummary(state = state, actions = actions)

            else -> RunBody(state = state, actions = actions)
        }
    }
}

@Composable
private fun RunBody(
    state: ImportRunUiState,
    actions: ImportRunActions,
) {
    Column(modifier = Modifier.fillMaxSize()) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
            verticalArrangement = Arrangement.spacedBy(FooterSpacing),
        ) {
            Text(
                text =
                    stringResource(R.string.android_importer_run_progress).withArguments(
                        "completed" to state.progression.completed,
                        "total" to state.progression.total,
                    ),
                style = MaterialTheme.typography.titleMediumEmphasized,
            )
            CapsuleProgress(
                progress = { state.progression.fraction },
                modifier = Modifier.fillMaxWidth(),
            )
            if (state.isStreaming) {
                Text(
                    text =
                        stringResource(R.string.android_importer_run_window)
                            .withArguments("count" to state.progression.windowSize),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(IMPORT_WINDOW_TAG),
                )
            }
            if (state.isPaused) {
                CapsuleBanner(
                    title = stringResource(R.string.android_importer_run_paused_title),
                    modifier = Modifier.fillMaxWidth().testTag(IMPORT_PAUSED_TAG),
                    body = stringResource(R.string.android_importer_run_paused_body),
                    severity = CapsuleBannerSeverity.Degraded,
                )
            }
            if (state.failure != null) {
                CapsuleBanner(
                    title = stringResource(R.string.android_importer_run_failure_title),
                    modifier = Modifier.fillMaxWidth(),
                    body = localizedErrorMessage(state.failure),
                    severity = CapsuleBannerSeverity.Error,
                    action = {
                        CapsuleButton(
                            onClick = actions.onRetry,
                            label = stringResource(R.string.android_importer_retry),
                            emphasis = CapsuleButtonEmphasis.Secondary,
                        )
                    },
                )
            }
        }
        LazyColumn(modifier = Modifier.weight(1f).testTag(IMPORT_RUN_FILES_TAG)) {
            items(state.progression.rows, key = ImportFileRow::locator) { row -> FileRow(row) }
        }
        CapsuleButton(
            onClick = actions.onCancel,
            label = stringResource(R.string.android_importer_run_cancel),
            modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
            emphasis = CapsuleButtonEmphasis.Tertiary,
        )
    }
}

@Composable
private fun FileRow(row: ImportFileRow) {
    val stateLabel = fileStateLabel(row.state)
    CapsuleListItem(
        headline = row.displayName,
        supporting = stateLabel,
        trailing = { CapsuleBadge(tone = row.state.tone(), contentDescription = stateLabel) },
        contentDescription =
            stringResource(R.string.android_importer_file_description)
                .withArguments("name" to row.displayName, "state" to stateLabel),
    )
}

/** The badge tone one file's state deserves — failure is the only one worth a colour. */
private fun ImportFileState.tone(): CapsuleBadgeTone =
    when (this) {
        ImportFileState.Done -> CapsuleBadgeTone.Success
        ImportFileState.Failed -> CapsuleBadgeTone.SyncFailed
        else -> CapsuleBadgeTone.SyncPending
    }

@Composable
private fun ImportSummary(
    state: ImportRunUiState,
    actions: ImportRunActions,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(ScreenPadding).testTag(IMPORT_SUMMARY_TAG),
        verticalArrangement = Arrangement.spacedBy(FooterSpacing),
    ) {
        Text(
            text =
                if (state.isCancelled) {
                    stringResource(R.string.android_importer_run_cancelled_title)
                } else {
                    stringResource(R.string.android_importer_summary_title)
                },
            style = MaterialTheme.typography.headlineSmallEmphasized,
        )
        if (state.isCancelled) {
            Text(
                text = stringResource(R.string.android_importer_run_cancelled_body),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        CapsuleCard(modifier = Modifier.fillMaxWidth()) {
            SummaryRow(
                label = stringResource(R.string.android_importer_summary_imported),
                value = state.progression.imported,
            )
            SummaryRow(
                label = stringResource(R.string.android_importer_summary_failed),
                value = state.progression.failed,
            )
            SummaryRow(
                label = stringResource(R.string.android_importer_summary_skipped),
                value = state.skippedCount,
            )
        }
        CapsuleButton(
            onClick = actions.onOpenLibrary,
            label = stringResource(R.string.android_importer_summary_view_library),
            modifier = Modifier.fillMaxWidth(),
            emphasis = CapsuleButtonEmphasis.Primary,
        )
    }
}

@Composable
private fun SummaryRow(
    label: String,
    value: Int,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(text = label, style = MaterialTheme.typography.bodyMedium)
        Text(text = value.toString(), style = MaterialTheme.typography.titleMediumEmphasized)
    }
}
