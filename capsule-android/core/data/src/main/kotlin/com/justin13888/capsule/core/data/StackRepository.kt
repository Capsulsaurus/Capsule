package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.GroupCullState
import com.justin13888.capsule.core.model.Stack
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.StackMember
import com.justin13888.capsule.core.model.StackMemberRole
import com.justin13888.capsule.core.model.StackType
import kotlinx.coroutines.flow.Flow

/**
 * Stacks — RAW+JPEG pairs, bursts, Live Photos, and the rest.
 *
 * Every operation here is **metadata-only**. Choosing a burst's best photo moves a pointer;
 * leaving a stack writes a stamped absence. Nothing on this interface can delete, rewrite, or
 * merge an original, which is what makes a buggy or hostile stack edit unable to lose bytes.
 */
interface StackRepository {
    /** One stack, re-emitted as its members, primary, or collapse state change. */
    fun stack(id: StackId): Flow<Stack?>

    /** A stack's members in sequence order. Mirrors `Catalog.list_stack_members`. */
    suspend fun stackMembers(stackId: StackId): List<StackMember>

    /**
     * Groups assets into a new stack.
     *
     * [primaryAssetId] must be one of [assetIds]; a stack whose primary is not a member would
     * collapse to something the user cannot see.
     */
    suspend fun createStack(
        stackType: StackType,
        primaryAssetId: AssetId,
        assetIds: List<AssetId>,
    ): StackId

    /** Adds an asset to an existing stack under a role. */
    suspend fun addMember(
        stackId: StackId,
        assetId: AssetId,
        role: StackMemberRole,
    )

    /** Removes an asset from its stack — a stamped absence on its membership register. */
    suspend fun removeMember(
        stackId: StackId,
        assetId: AssetId,
    )

    /**
     * Points the stack at a different primary. Mirrors `Catalog.update_stack_primary`.
     *
     * The "best photo" choice, and nothing more: the previous primary is untouched.
     */
    suspend fun setStackPrimary(
        stackId: StackId,
        primaryAssetId: AssetId,
    )

    /**
     * Suppresses or unsuppresses an asset as a non-primary stack member.
     *
     * Mirrors `Catalog.update_stack_hidden`. Distinct from an asset's own hidden flag —
     * un-stacking must not un-hide something the user deliberately hid.
     */
    suspend fun setStackHidden(
        assetId: AssetId,
        hidden: Boolean,
    )

    /** Collapses or expands a stack in the grid. */
    suspend fun setCollapsed(
        stackId: StackId,
        collapsed: Boolean,
    )

    /**
     * The stack's cull state, derived from its members.
     *
     * Derived on read rather than stored, so it can never disagree with the members it
     * summarises.
     */
    fun groupCullState(stackId: StackId): Flow<GroupCullState>
}
