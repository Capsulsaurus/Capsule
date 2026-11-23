package com.justin13888.capsule.feature.recovery

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.RecoveryRepository
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Duration

/**
 * The recovery hub's state machine.
 *
 * Read-mostly on purpose: the only write it can make is a snooze, and a snooze suppresses the
 * ask and nothing else. There is no method on this class that could refuse anything to
 * anyone, which is how "the cadence never blocks a critical flow" stays true as the feature
 * grows rather than being a comment somebody has to keep honouring.
 *
 * The collection runs in a job this class owns rather than in a `stateIn` pipeline because
 * retrying has to be able to *restart* it: a failure terminates the flow, and a pipeline that
 * could not be restarted would leave the error state with a retry button that did nothing.
 */
class RecoveryStatusViewModel(
    private val recovery: RecoveryRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(RecoveryStatusUiState())

    /** Everything the screen draws. */
    val state: StateFlow<RecoveryStatusUiState> = mutableState.asStateFlow()

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
                    .recoveryState()
                    .catch { throwable ->
                        mutableState.update {
                            it.copy(isLoading = false, failure = RecoveryFailure.of(throwable))
                        }
                    }.collect { current ->
                        mutableState.update { it.copy(isLoading = false, failure = null, recovery = current) }
                    }
            }
    }

    /** Defers the next prompt by [by]. Suppresses the ask, never the requirement. */
    fun snooze(by: Duration) {
        if (mutableState.value.snoozing) return
        mutableState.update { it.copy(snoozing = true, failure = null) }
        viewModelScope.launch {
            runCatching { recovery.snoozeVerification(by) }
                .onFailure { throwable ->
                    mutableState.update { it.copy(failure = RecoveryFailure.of(throwable)) }
                }
            mutableState.update { it.copy(snoozing = false) }
        }
    }

    companion object {
        /** The shorter of the two offered deferrals. */
        val SNOOZE_DAY: Duration = Duration.ofHours(24)

        /** The longer one. Both come straight from the cadence the design fixes. */
        val SNOOZE_WEEK: Duration = Duration.ofDays(7)
    }
}
