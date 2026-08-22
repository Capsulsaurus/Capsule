package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.StorageRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.OriginalFetchScenario
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.QuotaStatus
import com.justin13888.capsule.core.model.SyncState
import kotlinx.coroutines.flow.Flow

/**
 * Server-side quota and local cache occupancy — the two ways a library runs out of room.
 *
 * [verifyDurable] is the reason this is a separate contract from sync, and the fake treats it
 * with the same care the real one must: it is the last check before a device-owned original's
 * only copy is released, so it answers `false` whenever the server has not confirmed the
 * bytes rather than whenever it is convenient.
 */
class FakeStorageRepository(
    private val backend: MockBackend,
) : StorageRepository {
    override fun quota(): Flow<QuotaStatus> = backend.observe { _, scenario -> backend.quotaStatus(scenario) }

    override fun cacheBytes(): Flow<Long> = backend.observe { state, _ -> state.cacheBytes }

    override suspend fun evictCache(targetBytes: Long): Long {
        backend.gate(MockOperation.StorageWrite)
        return backend.mutate { state ->
            // Only re-fetchable, server-origin blobs are candidates. Pinned assets and
            // device-owned originals that have not been verified durable are never evicted,
            // however tight the budget — which is why the floor is a function of what is
            // pinned rather than of the target alone.
            val floor = state.pinned.size.toLong() * PINNED_BYTES_PER_ASSET
            val keep = maxOf(targetBytes, floor)
            val freed = (state.cacheBytes - keep).coerceAtLeast(0)
            state.cacheBytes -= freed
            freed
        }
    }

    override suspend fun verifyDurable(id: AssetId): Boolean {
        backend.gate(MockOperation.StorageWrite)
        val asset = backend.requireAsset(id)
        val scenario = backend.scenario.value
        // A `false` verdict means the local copy must be kept. An asset still waiting for its
        // original to land is exactly that case, and so is the whole library while the
        // scenario says originals have not left their device.
        if (scenario.originalFetch == OriginalFetchScenario.AwaitingUpload) return false
        return asset.syncState == SyncState.Synced
    }

    override fun heldRepresentations(id: AssetId): Flow<HeldRepresentations> =
        backend.observe { state, _ ->
            state.assets[id]?.heldRepresentations ?: HeldRepresentations.LQIP_ONLY
        }

    override suspend fun setPinned(
        id: AssetId,
        pinned: Boolean,
    ) {
        backend.gate(MockOperation.StorageWrite)
        backend.requireAsset(id)
        backend.mutate { state -> if (pinned) state.pinned += id else state.pinned -= id }
    }

    private companion object {
        /** What one pinned original costs the cache, for the eviction floor. */
        const val PINNED_BYTES_PER_ASSET = 6L * 1024 * 1024
    }
}
