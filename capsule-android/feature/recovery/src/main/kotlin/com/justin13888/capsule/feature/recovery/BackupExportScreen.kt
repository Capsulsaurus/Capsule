package com.justin13888.capsule.feature.recovery

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.BackupArtifactSummary

/** Test handle for the whole export surface. */
const val RECOVERY_EXPORT_TAG: String = "recovery.export"

/** Test handle for the destination field. */
const val RECOVERY_EXPORT_FIELD_TAG: String = "recovery.export.destination"

/** Test handle for the success state. */
const val RECOVERY_EXPORT_DONE_TAG: String = "recovery.export.done"

/** Test handle for the error state. */
const val RECOVERY_EXPORT_ERROR_TAG: String = "recovery.export.error"

/** Test handle for the empty state. */
const val RECOVERY_EXPORT_EMPTY_TAG: String = "recovery.export.empty"

/** Test handle for the loading state. */
const val RECOVERY_EXPORT_LOADING_TAG: String = "recovery.export.loading"

/** Test handle for one known artifact: `recovery.backup:<id>`. */
fun recoveryBackupTag(artifactId: String): String = "recovery.backup:$artifactId"

/**
 * Writing one encrypted archive of the library out.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. The copy names the one key that opens the archive — the recovery passphrase —
 * because an archive whose owner cannot say what unlocks it is an archive that will be
 * carried around for years and then found to be useless.
 *
 * @param state everything to draw.
 * @param onDestinationChange edits where the archive goes.
 * @param onExport writes it.
 * @param onReset puts the form back after a refusal.
 * @param onRetry re-reads the list of known artifacts.
 */
@Composable
fun BackupExportScreen(
    state: BackupExportUiState,
    onDestinationChange: (String) -> Unit,
    onExport: () -> Unit,
    onReset: () -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(RECOVERY_EXPORT_TAG)) {
        CapsuleTopAppBar(title = stringResource(R.string.android_destination_backup_export))
        Column(
            modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
            verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
        ) {
            Text(
                text = stringResource(R.string.android_recovery_export_body),
                style = MaterialTheme.typography.bodyMedium,
            )
            when (state.phase) {
                ExportPhase.Working -> ExportWorking()
                ExportPhase.Done -> ExportDone(state)
                ExportPhase.Failed -> ExportFailed(state, onReset)
                ExportPhase.Editing -> ExportForm(state, onDestinationChange, onExport)
            }
            KnownArtifacts(state, onRetry)
        }
    }
}

@Composable
private fun ExportForm(
    state: BackupExportUiState,
    onDestinationChange: (String) -> Unit,
    onExport: () -> Unit,
) {
    OutlinedTextField(
        value = state.destination,
        onValueChange = onDestinationChange,
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_EXPORT_FIELD_TAG),
        label = { Text(text = stringResource(R.string.android_recovery_export_destination_label)) },
        supportingText = {
            if (state.destinationMissing) {
                Text(text = stringResource(R.string.android_recovery_export_destination_required))
            }
        },
        isError = state.destinationMissing,
        singleLine = true,
        colors = capsuleFieldColors(),
    )
    CapsuleButton(
        onClick = onExport,
        label = stringResource(R.string.android_recovery_export_run),
        enabled = state.canExport,
    )
}

@Composable
private fun ExportWorking() {
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth(),
        title = stringResource(R.string.android_recovery_export_working),
        contentDescription = stringResource(R.string.android_recovery_export_working),
    )
}

@Composable
private fun ExportDone(state: BackupExportUiState) {
    CapsuleBanner(
        title = stringResource(R.string.android_recovery_export_done_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_EXPORT_DONE_TAG),
        body = state.exported?.let { artifactSummary(it) },
        severity = CapsuleBannerSeverity.Info,
    )
}

@Composable
private fun ExportFailed(
    state: BackupExportUiState,
    onReset: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_recovery_export_error_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_EXPORT_ERROR_TAG),
        body = localizedErrorMessage(state.failure?.code),
        action = {
            CapsuleButton(
                onClick = onReset,
                label = stringResource(R.string.android_recovery_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

/** The counts and the size, as this client computed them from the manifest it read. */
@Composable
internal fun artifactSummary(artifact: BackupArtifactSummary): String =
    stringResource(R.string.android_recovery_artifact_summary)
        .withArguments("count" to artifact.entryCount, "size" to formattedBytes(artifact.totalBytes))

@Composable
private fun KnownArtifacts(
    state: BackupExportUiState,
    onRetry: () -> Unit,
) {
    CapsuleSectionHeader(title = stringResource(R.string.android_recovery_export_existing))
    when {
        state.isLoading ->
            CapsuleLoadingState(
                modifier = Modifier.fillMaxWidth().testTag(RECOVERY_EXPORT_LOADING_TAG),
                title = stringResource(R.string.android_recovery_export_loading),
                contentDescription = stringResource(R.string.android_recovery_export_loading),
            )

        state.failure != null && state.phase != ExportPhase.Failed ->
            CapsuleErrorState(
                title = stringResource(R.string.android_recovery_error_title),
                modifier = Modifier.fillMaxWidth(),
                body = localizedErrorMessage(state.failure.code),
                action = {
                    CapsuleButton(
                        onClick = onRetry,
                        label = stringResource(R.string.android_recovery_retry),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                },
            )

        state.isEmpty ->
            CapsuleEmptyState(
                title = stringResource(R.string.android_recovery_export_empty_title),
                modifier = Modifier.fillMaxWidth().testTag(RECOVERY_EXPORT_EMPTY_TAG),
                body = stringResource(R.string.android_recovery_export_empty_body),
            )

        else -> state.backups.forEach { artifact -> ArtifactRow(artifact) }
    }
}

@Composable
private fun ArtifactRow(artifact: BackupArtifactSummary) {
    val exported =
        stringResource(R.string.android_recovery_artifact_exported)
            .withArguments("date" to formattedDate(artifact.exportedAt))
    val summary = artifactSummary(artifact)
    CapsuleListItem(
        headline = exported,
        modifier = Modifier.testTag(recoveryBackupTag(artifact.id.value)),
        supporting = summary,
        contentDescription = exported + SEPARATOR + summary,
    )
}
