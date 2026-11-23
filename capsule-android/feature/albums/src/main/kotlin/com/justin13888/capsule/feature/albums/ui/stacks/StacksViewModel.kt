package com.justin13888.capsule.feature.albums.ui.stacks

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.StackRepository
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.StackType
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.feature.albums.data.LibrarySample
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** One stack, as the listing shows it. */
data class StackRow(
    val id: StackId,
    /** What kind of grouping it is. A token from a newer peer still gets a row. */
    val stackType: Wire<StackType>,
    /** How many members the catalog holds for it. */
    val memberCount: Int,
    /** The member a collapsed stack shows, for the thumbnail. */
    val cover: Asset,
)

/** What the stacks listing is showing. */
sealed interface StacksUiState {
    /** Sampling the library and reading each stack's membership. */
    data object Loading : StacksUiState

    /** The read failed; [code] is the stable catalogue code. */
    data class Failed(
        val code: String?,
    ) : StacksUiState

    /** The stacks found, largest first. */
    data class Content(
        val rows: ImmutableList<StackRow> = persistentListOf(),
    ) : StacksUiState
}

/**
 * Every group of related files the library holds.
 *
 * Derived from the timeline rather than from a stack listing, because the catalog surface has
 * no "all stacks" query — and does not need one. A collapsed stack shows exactly one member,
 * that member is the only one in default views, and so walking the timeline finds every stack
 * exactly once. The suppressed members are then read per stack, which is also the only place
 * their count is authoritative.
 */
class StacksViewModel(
    private val stacks: StackRepository,
    private val sample: LibrarySample,
) : ViewModel() {
    private val state = MutableStateFlow<StacksUiState>(StacksUiState.Loading)

    /** What the listing is showing right now. */
    val uiState: StateFlow<StacksUiState> = state.asStateFlow()

    init {
        load()
    }

    /** Re-reads the sample and each stack's membership. */
    fun load() {
        viewModelScope.launch {
            state.value =
                try {
                    StacksUiState.Content(rows().toImmutableList())
                } catch (failure: CapsuleError) {
                    StacksUiState.Failed(failure.code?.value)
                }
        }
    }

    private suspend fun rows(): List<StackRow> {
        val covers =
            sample
                .assets()
                .assets
                .mapNotNull { asset -> asset.stack?.let { membership -> membership.stackId to asset } }
                .distinctBy { it.first }
                .take(MAX_STACKS)
        return covers
            .map { (stackId, cover) ->
                StackRow(
                    id = stackId,
                    stackType = requireNotNull(cover.stack).stackType,
                    memberCount = stacks.stackMembers(stackId).size,
                    cover = cover,
                )
            }.sortedWith(compareByDescending(StackRow::memberCount).thenBy { it.id.value })
    }

    private companion object {
        /**
         * How many stacks the listing reads membership for.
         *
         * One catalog call per stack, so the cap is what keeps opening this screen from
         * costing a query per burst in a five-year library.
         */
        const val MAX_STACKS = 64
    }
}
