package com.justin13888.capsule.feature.moderation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.ModerationRepository
import com.justin13888.capsule.core.model.UserBlock
import com.justin13888.capsule.core.model.UserId
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The blocked list's state machine.
 *
 * The collection runs in a job this class owns rather than in a `stateIn` pipeline because
 * retrying has to be able to *restart* it: a failure terminates the flow, and a pipeline that
 * could not be restarted would leave the error state with a retry button that did nothing.
 *
 * Blocks are the user's own and nothing here treats them otherwise — there is no path in this
 * class that publishes one, escalates it to a server-level decision, or tells the blocked
 * account it happened.
 */
class BlocksViewModel(
    private val moderation: ModerationRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(BlocksUiState())

    /** Everything the screen draws. */
    val state: StateFlow<BlocksUiState> = mutableState.asStateFlow()

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
                moderation
                    .blocks()
                    .catch { throwable ->
                        mutableState.update {
                            it.copy(isLoading = false, failure = ModerationFailure.of(throwable))
                        }
                    }.collect { blocks ->
                        mutableState.update {
                            it.copy(
                                isLoading = false,
                                failure = null,
                                blocks = blocks.sortedByDescending(UserBlock::createdAt),
                            )
                        }
                    }
            }
    }

    /** Edits the account to block. */
    fun onDraftChange(value: String) {
        mutableState.update { it.copy(draft = value, draftMissing = false) }
    }

    /**
     * Blocks the account named in the draft field.
     *
     * Refused locally when the field is empty rather than sent and rejected, so the user is
     * told what is missing instead of watching a call fail for reasons they cannot see.
     */
    fun block() {
        val id = mutableState.value.draft.trim()
        if (id.isEmpty()) {
            mutableState.update { it.copy(draftMissing = true) }
            return
        }
        write(id) { moderation.blockUser(UserId(id)) }
        mutableState.update { it.copy(draft = "") }
    }

    /** Lifts a block. Idempotent on the repository, so a double tap is harmless. */
    fun unblock(id: String) {
        write(id) { moderation.unblockUser(UserId(id)) }
    }

    /**
     * Runs one per-account write, marking that row busy for its duration.
     *
     * The list is not reloaded afterwards: the repository publishes the new list through the
     * flow this class is already collecting, so re-reading would be a second source of truth
     * that could disagree with the first.
     */
    private fun write(
        id: String,
        action: suspend () -> Unit,
    ) {
        if (id in mutableState.value.pending) return
        mutableState.update { it.copy(pending = it.pending + id, failure = null) }
        viewModelScope.launch {
            runCatching { action() }
                .onFailure { throwable ->
                    mutableState.update { it.copy(failure = ModerationFailure.of(throwable)) }
                }
            mutableState.update { it.copy(pending = it.pending - id) }
        }
    }
}
