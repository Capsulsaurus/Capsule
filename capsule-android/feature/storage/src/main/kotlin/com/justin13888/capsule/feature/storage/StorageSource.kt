package com.justin13888.capsule.feature.storage

import com.justin13888.capsule.core.data.StorageRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.QuotaStatus
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map

/**
 * One reading of everything the storage screens are about.
 *
 * A single value rather than four flows the screens each collect, so the quota figure, the
 * cache figure and the consumer list on screen at the same moment all come from the same
 * moment.
 */
data class StorageSnapshot(
    /** What the server accounts to this uploader. */
    val quota: QuotaStatus,
    /** What the reclaimable cache occupies here. */
    val cacheBytes: Long,
    /** How much of each new asset this device fetches. */
    val syncScope: SyncScope,
    /** The heaviest local consumers, heaviest first. */
    val consumers: List<ConsumerFacts>,
)

/**
 * One consumer as the repositories describe it, before the user's own decisions are folded in.
 *
 * Split from [StorageConsumer] because pinning and release progress are decisions this
 * feature holds, not facts a repository reports — merging them would make the derivation
 * depend on state it does not own and cannot recompute.
 */
data class ConsumerFacts(
    /** Which asset. */
    val id: AssetId,
    /** The asset itself, for the facts a row renders. */
    val asset: Asset,
    /** Its estimated local footprint. */
    val bytes: Long,
)

/**
 * Assembles [StorageSnapshot] from the three contracts that answer the question.
 *
 * A `single` collaborator rather than logic in the view model: the derivation is pure over
 * its inputs, it is the part worth testing on its own, and both storage destinations want the
 * same reading.
 */
class StorageSource(
    private val storage: StorageRepository,
    private val sync: SyncRepository,
    private val timeline: TimelineRepository,
) {
    /**
     * Re-emits whenever quota, cache occupancy, scope, or the library itself changes.
     *
     * The consumer scan re-reads a bounded window of the timeline rather than the whole
     * library: the list only ever shows the heaviest handful, and a library of a million
     * assets cannot be ranked in a recomposition. The window is the honest limit of the
     * answer, and it is stated in the type by [CONSUMER_SCAN] rather than hidden.
     */
    fun snapshots(): Flow<StorageSnapshot> =
        combine(
            storage.quota(),
            storage.cacheBytes(),
            sync.syncScope(),
            timeline.revision(),
        ) { quota, cacheBytes, scope, _ ->
            Triple(quota, cacheBytes, scope)
        }.map { (quota, cacheBytes, scope) ->
            StorageSnapshot(
                quota = quota,
                cacheBytes = cacheBytes,
                syncScope = scope,
                consumers = scanConsumers(),
            )
        }

    /**
     * The heaviest assets this device is currently holding bytes for.
     *
     * Assets holding nothing above the placeholder are excluded: the LQIP rides inside the
     * metadata blob, so listing one as a storage consumer would offer the user a row that
     * cannot free anything.
     */
    private suspend fun scanConsumers(): List<ConsumerFacts> =
        timeline
            .timelineWindow(offset = 0, limit = CONSUMER_SCAN)
            .filter { asset -> asset.heldRepresentations.best.rank > RepresentationTier.Lqip.rank }
            .map { asset -> ConsumerFacts(asset.id, asset, LocalFootprint.bytesOf(asset)) }
            .sortedByDescending(ConsumerFacts::bytes)
            .take(CONSUMER_LIMIT)

    companion object {
        /** How much of the timeline one scan reads. */
        const val CONSUMER_SCAN: Int = 240

        /** How many rows the list shows. */
        const val CONSUMER_LIMIT: Int = 12
    }
}

/** The asset's type where this build recognises the token, or `null` where it does not. */
internal fun Asset.knownType() = assetType.knownOrNull
