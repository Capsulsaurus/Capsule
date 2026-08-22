package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.UploadPolicy
import kotlinx.coroutines.flow.Flow
import java.time.Instant

/**
 * What has reached the server, what has not, and what the network currently allows.
 *
 * There is no `pause` or `resume` here, and that is deliberate: every background window is
 * preemptible by construction, work is chunked so an OS kill loses at most one chunk, and a
 * window that ends mid-reconciliation simply leaves the remainder for the next one. There is
 * no "must finish" state to pause, so exposing one would invite a caller to depend on a
 * guarantee the platform does not offer.
 */
interface SyncRepository {
    /** The device's current path quality, re-evaluated continuously. */
    fun connectionClass(): Flow<ConnectionClass>

    /** Where one asset stands relative to the server. */
    fun assetSyncState(id: AssetId): Flow<SyncState>

    /** When a reconciliation last completed, or `null` if none ever has. */
    fun lastCompletedSync(): Flow<Instant?>

    /** How much of each new asset this library fetches by default. */
    fun syncScope(): Flow<SyncScope>

    /** Changes the fetch scope. Lowering it does not evict what is already held. */
    suspend fun setSyncScope(scope: SyncScope)

    /** Whether upload sessions open eagerly or in tier order. */
    fun uploadPolicy(): Flow<UploadPolicy>

    /**
     * Changes the upload policy.
     *
     * Refused for an import already running in streaming mode: streaming exists to release
     * local bytes quickly and staged defers exactly the upload that release depends on, so
     * the two are mutually exclusive.
     */
    suspend fun setUploadPolicy(policy: UploadPolicy)

    /**
     * Runs a reconciliation now, with the user's explicit consent to ignore the metered and
     * Wi-Fi criteria.
     *
     * Only ever user-initiated. Background work never bypasses the connection rules, because
     * the user cannot consent to a data charge they were not shown.
     */
    suspend fun forceSyncNow()

    /**
     * Ensures a representation is available locally, returning what is actually in hand.
     *
     * The return value is the point: a permanent failure degrades to the best held rung
     * rather than throwing, so the caller draws *something* and the asset stays listed. Only
     * a failure that leaves nothing to draw — which cannot happen, since the placeholder is
     * always present — would be an error.
     */
    suspend fun ensureRepresentation(
        id: AssetId,
        wanted: RepresentationTier,
    ): RepresentationTier
}
