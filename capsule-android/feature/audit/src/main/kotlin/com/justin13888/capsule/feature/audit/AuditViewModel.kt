package com.justin13888.capsule.feature.audit

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The audit log's state machine.
 *
 * One immutable [AuditUiState] out and nothing else, so both panes of the list-detail layout
 * render from the same value and cannot disagree about what is selected.
 *
 * The collection runs in a job the view model owns rather than in a `stateIn` pipeline
 * because retrying has to be able to *restart* it: the log is assembled by reading the
 * repositories, and a failure there terminates the flow. A pipeline that could not be
 * restarted would leave the error state with a retry button that did nothing.
 */
class AuditViewModel(
    private val source: AuditLogSource,
) : ViewModel() {
    private val mutableState = MutableStateFlow(AuditUiState())

    /** Everything the screens draw. */
    val state: StateFlow<AuditUiState> = mutableState.asStateFlow()

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
                source
                    .snapshots()
                    .catch { throwable -> mutableState.update { it.failed(throwable) } }
                    .collect { snapshot -> mutableState.update { it.loaded(snapshot) } }
            }
    }

    /**
     * Includes or excludes one origin.
     *
     * Deselecting the last category is allowed and produces a visibly empty list with a way
     * back, rather than being silently reinterpreted as "everything" — a filter that quietly
     * ignores the user is a filter they stop trusting.
     */
    fun toggleCategory(category: AuditCategory) {
        mutableState.update { current ->
            val categories = current.filter.categories
            val next = if (category in categories) categories - category else categories + category
            current.copy(filter = current.filter.copy(categories = next))
        }
    }

    /** Narrows to records signed by this device, or widens back to every device. */
    fun toggleThisDevice() {
        mutableState.update { current ->
            val device = if (current.filter.onlyDevice == null) current.thisDevice else null
            current.copy(filter = current.filter.copy(onlyDevice = device))
        }
    }

    /** Puts every entry back in view. */
    fun clearFilters() {
        mutableState.update { it.copy(filter = AuditFilter()) }
    }

    /** Chooses the entry whose provenance record the detail pane shows. */
    fun select(entryId: String?) {
        mutableState.update { it.copy(selectedId = entryId) }
    }

    private fun AuditUiState.loaded(snapshot: AuditSnapshot): AuditUiState =
        copy(isLoading = false, failure = null, log = snapshot.log, devices = snapshot.devices)

    private fun AuditUiState.failed(throwable: Throwable): AuditUiState =
        copy(
            isLoading = false,
            failure = AuditFailure.of(throwable),
        )
}
