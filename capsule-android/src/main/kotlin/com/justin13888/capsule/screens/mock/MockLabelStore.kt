package com.justin13888.capsule.screens.mock

import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.AssetRegion
import com.justin13888.capsule.screens.domain.LabelPort
import com.justin13888.capsule.screens.domain.LabelTarget
import com.justin13888.capsule.screens.domain.LabelTargetNotFound
import com.justin13888.capsule.screens.domain.RegionId
import com.justin13888.capsule.screens.domain.RegionShape
import com.justin13888.capsule.screens.domain.StackId
import com.justin13888.capsule.screens.domain.UserLabel
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Region, stack and album labels, and the regions themselves, in memory.
 *
 * Refuses what the core's workspace refuses, in the same order: a label write refuses a
 * label the rule refuses before an unknown target, and a region write refuses a refused
 * first label, then a shape outside the normalized-coordinate contract, then an unknown
 * asset - and writes nothing on any refusal. Region ids are minted deterministically
 * from the seed, so two stores built alike mint the same ids, and a deleted id is never
 * minted again.
 *
 * Whether an asset, stack or album exists is the library's answer, asked through
 * [Existence], so a label can only land on something the library shows.
 */
class MockLabelStore(
    private val existence: Existence,
    private val seed: Long,
) : LabelPort {
    /** Whether the library holds an asset, a stack, or a container album. */
    class Existence(
        val asset: suspend (AssetId) -> Boolean,
        val stack: suspend (StackId) -> Boolean,
        val album: suspend (AlbumId) -> Boolean,
    ) {
        companion object {
            /** Every id exists: for a test of the store's own rules. */
            val EVERYTHING = Existence({ true }, { true }, { true })

            /** No id exists. */
            val NOTHING = Existence({ false }, { false }, { false })
        }
    }

    private val lock = Mutex()
    private var minted = 0
    private val regions = mutableMapOf<AssetId, MutableList<AssetRegion>>()
    private val stackLabels = mutableMapOf<StackId, MutableSet<String>>()
    private val albumLabels = mutableMapOf<AlbumId, MutableSet<String>>()

    override suspend fun labels(target: LabelTarget): List<String> =
        lock.withLock {
            requireExists(target)
            current(target)
        }

    override suspend fun addLabel(
        label: String,
        target: LabelTarget,
    ): List<String> {
        val normalized = UserLabel.normalize(label)
        return lock.withLock {
            requireExists(target)
            update(target) { it.add(normalized) }
            current(target)
        }
    }

    override suspend fun removeLabel(
        label: String,
        target: LabelTarget,
    ): List<String> {
        val normalized = UserLabel.normalize(label)
        return lock.withLock {
            requireExists(target)
            update(target) { it.remove(normalized) }
            current(target)
        }
    }

    override suspend fun regions(asset: AssetId): List<AssetRegion> =
        lock.withLock {
            if (!existence.asset(asset)) throw LabelTargetNotFound()
            regions[asset].orEmpty().toList()
        }

    override suspend fun addRegion(
        shape: RegionShape,
        labels: List<String>,
        asset: AssetId,
    ): AssetRegion {
        val normalized = labels.map(UserLabel::normalize)
        shape.validate()
        return lock.withLock {
            if (!existence.asset(asset)) throw LabelTargetNotFound()
            val region = AssetRegion(MockIdentifiers.regionId(seed, minted), shape, normalized.toSortedSet().toList())
            minted += 1
            regions.getOrPut(asset) { mutableListOf() }.add(region)
            region
        }
    }

    override suspend fun deleteRegion(
        region: RegionId,
        asset: AssetId,
    ) {
        lock.withLock {
            val held = regions[asset]
            if (!existence.asset(asset) || held == null || held.none { it.id == region }) throw LabelTargetNotFound()
            held.removeAll { it.id == region }
        }
    }

    private suspend fun requireExists(target: LabelTarget) {
        val exists =
            when (target) {
                is LabelTarget.Region -> existence.asset(target.asset) && regions[target.asset].orEmpty().any { it.id == target.region }
                is LabelTarget.Stack -> existence.stack(target.stack)
                is LabelTarget.Album -> existence.album(target.album)
            }
        if (!exists) throw LabelTargetNotFound()
    }

    private fun current(target: LabelTarget): List<String> =
        when (target) {
            is LabelTarget.Region -> regions[target.asset].orEmpty().firstOrNull { it.id == target.region }?.labels.orEmpty()
            is LabelTarget.Stack -> stackLabels[target.stack].orEmpty().sorted()
            is LabelTarget.Album -> albumLabels[target.album].orEmpty().sorted()
        }

    private fun update(
        target: LabelTarget,
        change: (MutableSet<String>) -> Unit,
    ) {
        when (target) {
            is LabelTarget.Region -> {
                val held = regions[target.asset] ?: return
                val index = held.indexOfFirst { it.id == target.region }
                if (index < 0) return
                val labels = held[index].labels.toMutableSet()
                change(labels)
                held[index] = held[index].copy(labels = labels.sorted())
            }
            is LabelTarget.Stack -> change(stackLabels.getOrPut(target.stack) { mutableSetOf() })
            is LabelTarget.Album -> change(albumLabels.getOrPut(target.album) { mutableSetOf() })
        }
    }
}
