package com.justin13888.capsule.feature.moderation

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.UserBlock

/**
 * Everything the blocked-list screen draws, as one value.
 *
 * [pending] is keyed by account rather than being a single boolean because blocking and
 * unblocking are per row and the list stays interactive while one is in flight: a screen-wide
 * spinner would forbid the second unblock while the first was still travelling, for no reason
 * other than that it was easier to model.
 */
@Immutable
data class BlocksUiState(
    /** Whether the first reading is still in flight. */
    val isLoading: Boolean = true,
    /** The failure to explain, or `null`. */
    val failure: ModerationFailure? = null,
    /** The accounts this user has blocked, newest first. */
    val blocks: List<UserBlock> = emptyList(),
    /** What the user has typed into the add field. */
    val draft: String = "",
    /** Whether the last add attempt named nothing. */
    val draftMissing: Boolean = false,
    /** Accounts with a block or unblock in flight. */
    val pending: Set<String> = emptySet(),
) {
    /** Nobody is blocked, and that is a settled fact rather than a pending read. */
    val isEmpty: Boolean get() = !isLoading && failure == null && blocks.isEmpty()

    /** Whether the add action is live. */
    val canAdd: Boolean get() = draft.isNotBlank() && draft.trim() !in pending

    /** Whether [id] has a write in flight. */
    fun isPending(id: String): Boolean = id in pending
}
