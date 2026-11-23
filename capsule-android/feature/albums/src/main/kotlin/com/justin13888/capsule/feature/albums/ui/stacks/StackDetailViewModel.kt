package com.justin13888.capsule.feature.albums.ui.stacks

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.StackRepository
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.Stack
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.StackMember
import com.justin13888.capsule.core.model.StackMemberRole
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/** One member of a stack, with what it is and where it sits. */
data class StackMemberRow(
    val member: StackMember,
    val asset: Asset?,
    /** Whether this is the member a collapsed stack shows. */
    val isPrimary: Boolean,
)

/** What stack detail is showing. */
sealed interface StackDetailUiState {
    /** Reading the stack and its members. */
    data object Loading : StackDetailUiState

    /** The read failed; [code] is the stable catalogue code. */
    data class Failed(
        val code: String?,
    ) : StackDetailUiState

    /** The stack, expanded into its members in sequence order. */
    data class Content(
        val stack: Stack,
        val members: ImmutableList<StackMemberRow> = persistentListOf(),
    ) : StackDetailUiState
}

/**
 * One stack, expanded.
 *
 * Every operation here is metadata-only by construction, which is the property that lets the
 * screen offer them without a confirmation: choosing a different best photo moves a pointer,
 * and reordering rewrites membership rows. Neither can delete, rewrite or merge an original,
 * so a mistake costs a tap to undo rather than a photograph.
 */
class StackDetailViewModel(
    private val stackId: StackId,
    private val stacks: StackRepository,
    private val assets: AssetRepository,
) : ViewModel() {
    private val state = MutableStateFlow<StackDetailUiState>(StackDetailUiState.Loading)

    /** What the screen is showing right now. */
    val uiState: StateFlow<StackDetailUiState> = state.asStateFlow()

    init {
        load()
    }

    /** Re-reads the stack and its members. */
    fun load() {
        viewModelScope.launch {
            state.value =
                try {
                    val stack = stacks.stack(stackId).first()
                    if (stack == null) {
                        StackDetailUiState.Failed(code = null)
                    } else {
                        StackDetailUiState.Content(stack = stack, members = rows(stack).toImmutableList())
                    }
                } catch (failure: CapsuleError) {
                    StackDetailUiState.Failed(failure.code?.value)
                }
        }
    }

    /** Points the stack at a different best photo. The previous primary is untouched. */
    fun setPrimary(assetId: AssetId) {
        viewModelScope.launch {
            runCatching { stacks.setStackPrimary(stackId, assetId) }
            load()
        }
    }

    /** Moves a member one place earlier in the sequence. */
    fun moveEarlier(index: Int) = move(index, index - 1)

    /** Moves a member one place later in the sequence. */
    fun moveLater(index: Int) = move(index, index + 1)

    /**
     * Rewrites the sequence so [from] lands at [to].
     *
     * The catalog surface has no reorder primitive — membership is an LWW register per asset,
     * and its ordering is the row's `sequence_order` — so the tail from the first changed
     * position is rewritten in place: each affected member leaves the stack (a stamped
     * absence) and rejoins in the new order. Every call is metadata-only, and the primary
     * pointer is restored afterwards because rejoining makes a member ordinary.
     */
    private fun move(
        from: Int,
        to: Int,
    ) {
        val current = state.value as? StackDetailUiState.Content ?: return
        if (from !in current.members.indices || to !in current.members.indices) return
        val reordered = current.members.map(StackMemberRow::member).toMutableList()
        reordered.add(to, reordered.removeAt(from))
        val tail = reordered.drop(minOf(from, to))
        viewModelScope.launch {
            runCatching {
                tail.forEach { member -> stacks.removeMember(stackId, member.assetId) }
                tail.forEach { member ->
                    stacks.addMember(
                        stackId = stackId,
                        assetId = member.assetId,
                        role = member.memberRole.knownOrNull ?: StackMemberRole.Alternate,
                    )
                }
                stacks.setStackPrimary(stackId, current.stack.primaryAssetId)
            }
            load()
        }
    }

    private suspend fun rows(stack: Stack): List<StackMemberRow> =
        stacks.stackMembers(stackId).map { member ->
            StackMemberRow(
                member = member,
                asset = assets.findByUuid(member.assetId),
                isPrimary = member.assetId == stack.primaryAssetId,
            )
        }
}
