package com.justin13888.capsule.feature.sync

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.ErrorCode
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** Everything the quarantine surface draws. */
data class QuarantineUiState(
    /** The first read is still in flight. */
    val isLoading: Boolean = true,
    /** What is currently held. */
    val items: ImmutableList<QuarantineItem> = persistentListOf(),
    /** The coded failure to localize, or `null`. */
    val failure: ErrorCode? = null,
    /** Set when a failure carries no catalogue code — a purely local refusal. */
    val hasLocalFailure: Boolean = false,
) {
    /** Whether nothing needs a decision. */
    val isEmpty: Boolean get() = !isLoading && items.isEmpty()

    /** Whether an error state should replace the content. */
    val hasFailed: Boolean get() = items.isEmpty() && (failure != null || hasLocalFailure)
}

/**
 * The quarantine surface's state machine.
 *
 * It has no "apply" and no "apply all", and that is the point of the whole surface: inbound
 * state that failed a check is never silently applied and never silently dropped. Every
 * decision here is one the user takes about one item, which is why [resolve] removes a row
 * rather than mutating whatever the row described.
 */
class QuarantineViewModel(
    private val source: QuarantineSource,
) : ViewModel() {
    private val mutableState = MutableStateFlow(QuarantineUiState())

    /** Everything the quarantine screen draws. */
    val state: StateFlow<QuarantineUiState> = mutableState.asStateFlow()

    init {
        reload()
    }

    /** Re-reads what is held. Also the error state's remediation. */
    fun reload() {
        mutableState.update { it.copy(isLoading = true, failure = null, hasLocalFailure = false) }
        viewModelScope.launch {
            runCatching { source.items() }
                .onSuccess { items ->
                    mutableState.update { it.copy(isLoading = false, items = items.toImmutableList()) }
                }.onFailure { throwable -> mutableState.update { it.copy(isLoading = false).failed(throwable) } }
        }
    }

    /**
     * Records the user's decision about one held item and drops it from the list.
     *
     * The decision itself is not written anywhere yet: `core:data` carries no quarantine
     * contract, so this cannot pretend to have discarded a manifest or merged a chain. What it
     * can do honestly is stop presenting a question the user has answered, and re-reading the
     * source will show it again if the refusal is still live.
     */
    fun resolve(item: QuarantineItem) {
        mutableState.update { current ->
            current.copy(items = current.items.filterNot { it.key == item.key }.toImmutableList())
        }
    }

    private fun QuarantineUiState.failed(throwable: Throwable): QuarantineUiState =
        copy(
            failure = (throwable as? CapsuleError)?.code,
            hasLocalFailure = throwable !is CapsuleError || throwable.code == null,
        )
}
