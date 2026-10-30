package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.StackId
import com.justin13888.capsule.screens.mock.MockIdentifiers

/**
 * [CullPort] over assets held in memory: the mock world's library, and the store the
 * culling model's tests drive. Its answers are what the library would answer, including a
 * stack flag reaching members the pass never read.
 *
 * Setting [failing] makes every write throw, and a sweep stop before moving anything; it
 * stands in for a store that cannot be reached.
 */
class InMemoryCullPort(
    assets: List<CullAsset> = emptyList(),
) : CullPort {
    private val live = assets.toMutableList()

    /** The assets moved to the trash, in the order they went. */
    val trashed = mutableListOf<AssetId>()

    /** Every write throws while this is set. */
    var failing = false

    /** The assets still live, with the flags they hold. */
    val assets: List<CullAsset> get() = live.toList()

    override suspend fun pass(): List<CullAsset> {
        check(!failing) { "the store is unreachable" }
        return live.toList()
    }

    override suspend fun setCull(
        asset: AssetId,
        flag: CullFlag,
    ) {
        check(!failing) { "the store is unreachable" }
        val at = live.indexOfFirst { it.id == asset }
        if (at >= 0) live[at] = live[at].copy(flag = flag)
    }

    override suspend fun flagStack(
        stack: StackId,
        flag: CullFlag,
    ): List<AssetId> {
        check(!failing) { "the store is unreachable" }
        val members = live.filter { it.stack?.stack == stack }.map { it.id }
        members.forEach { setCull(it, flag) }
        return members.sortedBy { it.value }
    }

    override suspend fun moveToTrash(assets: List<AssetId>): List<AssetId> {
        if (failing) throw SweepIncomplete(emptyList())
        val moved = assets.filter { id -> live.removeAll { it.id == id } }
        trashed.addAll(moved)
        return moved
    }

    companion object {
        /**
         * The mock world's pass: six lone photos and a three-photo burst in the middle, one
         * photo already picked and one already rejected.
         */
        fun sample(seed: Long = 0x5eed): InMemoryCullPort {
            val burst = MockIdentifiers.stackId(seed, 1)
            val lone = (0 until 6).map { CullAsset(MockIdentifiers.assetId(seed, 100 + it)) }
            val members =
                (0 until 3).map {
                    CullAsset(
                        MockIdentifiers.assetId(seed, 110 + it),
                        stack = StackMembership(burst, isCover = it == 0, memberIndex = it),
                    )
                }
            val picked = lone[1].copy(flag = CullFlag.PICK)
            val rejected = lone[4].copy(flag = CullFlag.REJECT)
            return InMemoryCullPort(listOf(lone[0], picked, lone[2]) + members + listOf(lone[3], rejected, lone[5]))
        }
    }
}
