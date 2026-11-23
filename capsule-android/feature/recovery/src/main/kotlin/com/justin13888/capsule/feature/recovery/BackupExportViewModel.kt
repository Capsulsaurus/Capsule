package com.justin13888.capsule.feature.recovery

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.RecoveryRepository
import com.justin13888.capsule.core.model.BackupArtifactSummary
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** Where an export has got to. */
enum class ExportPhase {
    /** Waiting for a destination. */
    Editing,

    /** Writing the archive. */
    Working,

    /** Written. */
    Done,

    /** Not written, and why is in [BackupExportUiState.failure]. */
    Failed,
}

/**
 * Everything the export screen draws, as one value.
 *
 * The list of known artifacts sits on the same screen as the action that adds to it, because
 * the question a user actually has here is "do I already have one, and how old is it" — and
 * answering that on a different screen is how people end up with either none or eleven.
 */
@Immutable
data class BackupExportUiState(
    /** Whether the first reading of known artifacts is still in flight. */
    val isLoading: Boolean = true,
    /** The failure to explain, or `null`. */
    val failure: RecoveryFailure? = null,
    /** Artifacts this device knows about, newest first. */
    val backups: List<BackupArtifactSummary> = emptyList(),
    /** Where the next archive should be written. */
    val destination: String = "",
    /** Whether the last attempt named nowhere. */
    val destinationMissing: Boolean = false,
    /** Where the export has got to. */
    val phase: ExportPhase = ExportPhase.Editing,
    /** What the last successful export wrote. */
    val exported: BackupArtifactSummary? = null,
) {
    /** No artifact has ever been written from this device. */
    val isEmpty: Boolean get() = !isLoading && failure == null && backups.isEmpty()

    /** Whether the export action is live. */
    val canExport: Boolean get() = phase != ExportPhase.Working
}

/**
 * The export screen's state machine.
 *
 * The collection of known artifacts runs in a job this class owns rather than in a `stateIn`
 * pipeline because retrying has to be able to *restart* it: a failure terminates the flow,
 * and a pipeline that could not be restarted would leave the error state with a retry button
 * that did nothing.
 */
class BackupExportViewModel(
    private val recovery: RecoveryRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(BackupExportUiState())

    /** Everything the screen draws. */
    val state: StateFlow<BackupExportUiState> = mutableState.asStateFlow()

    private var collection: Job? = null

    init {
        reload()
    }

    /** Restarts the reading. Also the remediation behind the error state's action. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null) }
        collection =
            viewModelScope.launch {
                recovery
                    .backups()
                    .catch { throwable ->
                        mutableState.update {
                            it.copy(isLoading = false, failure = RecoveryFailure.of(throwable))
                        }
                    }.collect { backups ->
                        mutableState.update {
                            it.copy(
                                isLoading = false,
                                failure = null,
                                backups = backups.sortedByDescending(BackupArtifactSummary::exportedAt),
                            )
                        }
                    }
            }
    }

    /** Edits where the archive should go. */
    fun onDestinationChange(value: String) {
        mutableState.update { it.copy(destination = value, destinationMissing = false) }
    }

    /**
     * Writes an archive.
     *
     * Refused locally when no destination is named rather than sent and rejected, so the user
     * is told what is missing instead of watching a call fail for reasons they cannot see.
     */
    fun export() {
        val current = mutableState.value
        if (current.phase == ExportPhase.Working) return
        if (current.destination.isBlank()) {
            mutableState.update { it.copy(destinationMissing = true) }
            return
        }
        mutableState.update { it.copy(phase = ExportPhase.Working, failure = null, destinationMissing = false) }
        viewModelScope.launch {
            runCatching { recovery.exportBackup(current.destination.trim()) }
                .onSuccess { summary ->
                    mutableState.update { it.copy(phase = ExportPhase.Done, exported = summary) }
                }.onFailure { throwable ->
                    mutableState.update {
                        it.copy(phase = ExportPhase.Failed, failure = RecoveryFailure.of(throwable))
                    }
                }
        }
    }

    /** Puts the form back in front of the user, with the destination intact. */
    fun reset() {
        mutableState.update { it.copy(phase = ExportPhase.Editing, failure = null) }
    }
}
