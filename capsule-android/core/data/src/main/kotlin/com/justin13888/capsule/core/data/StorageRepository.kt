package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.QuotaStatus
import kotlinx.coroutines.flow.Flow

/**
 * Server-side quota and local cache occupancy — the two ways a library runs out of room.
 *
 * [verifyDurable] is the verify-before-destroy gate and is the reason this interface exists
 * separately from [SyncRepository]. No path may release the only local copy of a
 * device-owned original until the server confirms it holds it durably, so the check is a
 * first-class call that returns a verdict rather than a side effect buried in an eviction
 * routine.
 */
interface StorageRepository {
    /** The uploader's standing against their limits. */
    fun quota(): Flow<QuotaStatus>

    /** Bytes the local blob cache currently occupies. */
    fun cacheBytes(): Flow<Long>

    /**
     * Evicts unpinned cached blobs until at most [targetBytes] remain, returning what was
     * freed.
     *
     * Only re-fetchable, server-origin blobs are candidates: discarding those is always safe
     * because they can be fetched again. A device-owned original that has not been verified
     * durable is never evicted, however tight the budget.
     */
    suspend fun evictCache(targetBytes: Long): Long

    /**
     * Asks the server to confirm it durably holds this asset's original.
     *
     * A `false` verdict means the local copy must be kept. Callers must not treat a failed
     * call as a `false` and a `false` as "probably fine" — this is the last check before
     * bytes become unrecoverable.
     */
    suspend fun verifyDurable(id: AssetId): Boolean

    /** Which rungs of the ladder are held locally for an asset. */
    fun heldRepresentations(id: AssetId): Flow<HeldRepresentations>

    /** Pins an asset's original so cache pressure cannot evict it. */
    suspend fun setPinned(
        id: AssetId,
        pinned: Boolean,
    )
}
