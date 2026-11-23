package com.justin13888.capsule.feature.importer

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CreateNewFolder
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCheckbox
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumId

/** Test handle for the source list. */
const val IMPORT_SOURCES_TAG: String = "importer.sources"

/** Test handle for the primary scan action. */
const val IMPORT_SCAN_TAG: String = "importer.scan"

/**
 * Every callback the source picker raises, in one object.
 *
 * A parameter object rather than eight parameters because the screen is stateless by
 * contract: it is composed from tests and previews as often as from the app, and a
 * seven-lambda call site is where those copies quietly drift apart.
 */
data class ImportSourcesActions(
    /** Ticks or unticks one source, by locator. */
    val onToggle: (String) -> Unit,
    /** Opens the system folder picker. */
    val onPickFolder: () -> Unit,
    /** Adds a tethered camera as a source. */
    val onAddCamera: () -> Unit,
    /** Scans the ticked sources, or raises the destination question first. */
    val onScan: () -> Unit,
    /** Highlights an album inside the destination question. */
    val onChoosePromptAlbum: (AlbumId) -> Unit,
    /** Answers the destination question. */
    val onConfirmPrompt: () -> Unit,
    /** Puts the destination question away unanswered. */
    val onDismissPrompt: () -> Unit,
    /** Re-reads the source list. */
    val onRetry: () -> Unit,
    /** Leaves the screen. */
    val onBack: () -> Unit,
)

/**
 * Choosing where an import reads from.
 *
 * Sources are listed by their **relative path**, which is the same identity the scope and its
 * remembered destination are keyed on. A row therefore means the same thing across a reinstall
 * and across two phones pointed at the same folder — which a `MediaStore` bucket id would not.
 */
@Composable
fun ImportSourcesScreen(
    state: ImportSourcesUiState,
    actions: ImportSourcesActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_import_sources),
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
                    title = stringResource(R.string.android_importer_sources_loading),
                    contentDescription = stringResource(R.string.android_importer_sources_loading),
                )

            state.hasFailed ->
                CapsuleErrorState(
                    title = stringResource(R.string.android_importer_sources_error_title),
                    modifier = Modifier.fillMaxSize(),
                    body = localizedErrorMessage(state.failure),
                    action = {
                        CapsuleButton(
                            onClick = actions.onRetry,
                            label = stringResource(R.string.android_importer_retry),
                        )
                    },
                )

            state.isEmpty ->
                CapsuleEmptyState(
                    title = stringResource(R.string.android_importer_sources_empty_title),
                    modifier = Modifier.fillMaxSize(),
                    body = stringResource(R.string.android_importer_sources_empty_body),
                    action = {
                        CapsuleButton(
                            onClick = actions.onPickFolder,
                            label = stringResource(R.string.android_importer_pick_folder),
                        )
                    },
                )

            else -> SourceList(state = state, actions = actions, modifier = Modifier.fillMaxSize())
        }
    }
    if (state.prompt != null) {
        ScopeMappingSheet(
            prompt = state.prompt,
            albums = state.albums,
            onChoose = actions.onChoosePromptAlbum,
            onConfirm = actions.onConfirmPrompt,
            onDismiss = actions.onDismissPrompt,
        )
    }
}

@Composable
private fun SourceList(
    state: ImportSourcesUiState,
    actions: ImportSourcesActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier) {
        LazyColumn(modifier = Modifier.weight(1f).testTag(IMPORT_SOURCES_TAG)) {
            item { CapsuleSectionHeader(title = stringResource(R.string.android_importer_section_device)) }
            item {
                Text(
                    text = stringResource(R.string.android_importer_sources_summary),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = ScreenPadding),
                )
            }
            items(state.sources, key = ImportSource::locator) { source ->
                SourceRow(
                    source = source,
                    selected = source.locator in state.selected,
                    destinationName = state.albums.firstOrNull { it.id == source.destination }?.name,
                    onToggle = { actions.onToggle(source.locator) },
                )
            }
            item { CapsuleSectionHeader(title = stringResource(R.string.android_importer_section_other)) }
            item {
                CapsuleListItem(
                    headline = stringResource(R.string.android_importer_source_takeout),
                    leading = { Icon(imageVector = Icons.Filled.CreateNewFolder, contentDescription = null) },
                    onClick = actions.onPickFolder,
                )
            }
            item {
                CapsuleListItem(
                    headline = stringResource(R.string.android_importer_source_tethered_camera),
                    leading = { Icon(imageVector = Icons.Filled.PhotoCamera, contentDescription = null) },
                    onClick = actions.onAddCamera,
                )
            }
        }
        ScanFooter(state = state, onScan = actions.onScan)
    }
}

@Composable
private fun SourceRow(
    source: ImportSource,
    selected: Boolean,
    destinationName: String?,
    onToggle: () -> Unit,
) {
    val label = sourceLabel(source)
    val description = stringResource(R.string.android_importer_source_description)
    val album = albumLabel(destinationName)
    CapsuleListItem(
        headline = label,
        modifier = Modifier.semantics { this.selected = selected },
        supporting = source.locator,
        overline =
            if (source.isMapped) {
                stringResource(R.string.android_importer_scope_mapped).withArguments("album" to album)
            } else {
                stringResource(R.string.android_importer_scope_unmapped)
            },
        leading = { CapsuleCheckbox(checked = selected, onCheckedChange = { onToggle() }) },
        onClick = onToggle,
        contentDescription = description.withArguments("name" to label, "path" to source.locator),
    )
}

@Composable
private fun ScanFooter(
    state: ImportSourcesUiState,
    onScan: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(FooterSpacing),
    ) {
        val scan = state.scan
        if (scan != null) {
            Text(
                text = stringResource(R.string.android_importer_scanning_title).withArguments("source" to scan.locator),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                text =
                    stringResource(R.string.android_importer_scan_progress)
                        .withArguments("count" to scan.filesFound),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        CapsuleButton(
            onClick = onScan,
            label =
                stringResource(R.string.android_importer_scan_action)
                    .withArguments("count" to state.selected.size),
            modifier = Modifier.fillMaxWidth().testTag(IMPORT_SCAN_TAG),
            emphasis = CapsuleButtonEmphasis.Primary,
            enabled = state.canScan,
        )
    }
}

/** The screen's outer inset, matching the design system's list padding. */
internal val ScreenPadding = 16.dp

/** The gap between the footer's progress lines and its action. */
internal val FooterSpacing = 8.dp
