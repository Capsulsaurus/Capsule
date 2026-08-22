package com.justin13888.capsule.navigation.entries

import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.SourceKind
import com.justin13888.capsule.feature.importer.ImportPlanActions
import com.justin13888.capsule.feature.importer.ImportPlanScreen
import com.justin13888.capsule.feature.importer.ImportPlanViewModel
import com.justin13888.capsule.feature.importer.ImportRunActions
import com.justin13888.capsule.feature.importer.ImportRunScreen
import com.justin13888.capsule.feature.importer.ImportRunViewModel
import com.justin13888.capsule.feature.importer.ImportSourcesActions
import com.justin13888.capsule.feature.importer.ImportSourcesScreen
import com.justin13888.capsule.feature.importer.ImportSourcesViewModel
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination
import org.koin.androidx.compose.koinViewModel
import org.koin.core.parameter.parametersOf

/**
 * Choosing a source, reviewing the plan, and watching the run.
 *
 * Three destinations rather than one wizard because an import survives the app being closed:
 * the user must be able to come back to a running import from anywhere, which needs a key
 * that names it.
 *
 * Each hand-off is driven by a one-shot field on the state rather than by a callback return —
 * `plannedImportId`, then `startedImportId` — and cleared as soon as it is acted on. A
 * navigation that fired from a composition would fire again on the next recomposition, which
 * on this flow means confirming the same plan twice.
 */
fun EntryProviderScope<NavKey>.importerEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.ImportSources>(scope) {
        val viewModel = koinViewModel<ImportSourcesViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        LaunchedEffect(state.plannedImportId) {
            val planned = state.plannedImportId ?: return@LaunchedEffect
            viewModel.clearPlan()
            scope.navigator.goTo(CapsuleDestination.ImportPlan(planned.value))
        }
        ImportSourcesScreen(
            state = state,
            actions =
                ImportSourcesActions(
                    onToggle = viewModel::toggle,
                    onPickFolder = { viewModel.addSource(PICKED_FOLDER_LOCATOR, SourceKind.Folder) },
                    onAddCamera = { viewModel.addSource(TETHERED_CAMERA_LOCATOR, SourceKind.RemovableVolume) },
                    onScan = viewModel::scan,
                    onChoosePromptAlbum = viewModel::choosePromptAlbum,
                    onConfirmPrompt = viewModel::confirmPrompt,
                    onDismissPrompt = viewModel::dismissPrompt,
                    onRetry = viewModel::reload,
                    onBack = { scope.navigator.goBack() },
                ),
        )
    }

    destination<CapsuleDestination.ImportPlan>(scope) { key ->
        val importId = ImportId(key.importId)
        val viewModel = koinViewModel<ImportPlanViewModel> { parametersOf(importId) }
        val state by viewModel.state.collectAsStateWithLifecycle()
        LaunchedEffect(state.startedImportId) {
            val started = state.startedImportId ?: return@LaunchedEffect
            viewModel.clearStarted()
            scope.navigator.goTo(CapsuleDestination.ImportProgress(started.value))
        }
        ImportPlanScreen(
            state = state,
            actions =
                ImportPlanActions(
                    onSetMode = viewModel::setMode,
                    onConfirm = viewModel::confirm,
                    onSwitchToFullUploads = viewModel::switchToFullUploads,
                    onRetry = viewModel::reload,
                    onBack = { scope.navigator.goBack() },
                ),
        )
    }

    destination<CapsuleDestination.ImportProgress>(scope) { key ->
        val importId = ImportId(key.importId)
        val viewModel = koinViewModel<ImportRunViewModel> { parametersOf(importId) }
        val state by viewModel.state.collectAsStateWithLifecycle()
        ImportRunScreen(
            state = state,
            actions =
                ImportRunActions(
                    onCancel = viewModel::cancel,
                    onRetry = viewModel::attach,
                    onOpenLibrary = { scope.navigator.switchTo(CapsuleDestination.Timeline) },
                    onBack = { scope.navigator.goBack() },
                ),
        )
    }
}

/**
 * The locator a hand-picked folder is filed under until the system picker supplies a real one.
 *
 * A relative path rather than a content URI, because that is the identity a scope is keyed on:
 * a URI carries a grant that expires, and a scope whose identity expired would ask the
 * destination question again every time.
 */
private const val PICKED_FOLDER_LOCATOR = "Pictures/Imported"

/** The locator a tethered camera's volume is filed under. */
private const val TETHERED_CAMERA_LOCATOR = "USB/DCIM"
