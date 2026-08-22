package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.AlbumError
import com.justin13888.capsule.core.data.BlobError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.data.UploadError
import com.justin13888.capsule.core.mock.InboundStateScenario
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.OriginalFetchScenario
import com.justin13888.capsule.core.mock.seed.DeterministicIds
import com.justin13888.capsule.core.mock.seed.Prng
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.QuarantineEntry
import com.justin13888.capsule.core.model.QuarantineId
import com.justin13888.capsule.core.model.QuarantineSurface
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.UploadPolicy
import com.justin13888.capsule.core.model.asWire
import kotlinx.coroutines.flow.Flow
import java.time.Instant

/**
 * What has reached the server, what has not, and what the network currently allows.
 *
 * There is no `pause` or `resume`, deliberately: every background window is preemptible by
 * construction and a window that ends mid-reconciliation simply leaves the remainder for the
 * next one. Exposing a pause would invite a caller to depend on a "must finish" guarantee the
 * platform does not offer.
 */
class FakeSyncRepository(
    private val backend: MockBackend,
) : SyncRepository {
    private val prng = Prng(Prng.mixSeed(backend.library.spec.seed, "sync"))

    override fun connectionClass(): Flow<ConnectionClass> = backend.observe { _, scenario -> scenario.connection }

    override fun assetSyncState(id: AssetId): Flow<SyncState> =
        backend.observe { state, scenario ->
            val stored = state.assets[id]?.syncState ?: SyncState.LocalOnly
            // Derived from the feed's `original_held` flag rather than stored twice: the
            // scenario stands in for the feed here, and the asset's own record stays the
            // single place the rest of its state lives.
            if (scenario.originalFetch == OriginalFetchScenario.AwaitingUpload && stored == SyncState.Synced) {
                SyncState.AwaitingOriginal
            } else {
                stored
            }
        }

    override fun lastCompletedSync(): Flow<Instant?> =
        backend.observe { state, scenario ->
            state.lastCompletedSync ?: scenario.lastSyncAgo?.let { backend.now().minus(it) }
        }

    override fun syncScope(): Flow<SyncScope> = backend.observe { state, _ -> state.settings.syncScope }

    override suspend fun setSyncScope(scope: SyncScope) {
        backend.gate(MockOperation.SyncControl)
        // Lowering the scope does not evict what is already held: the setting decides what is
        // fetched next, not what is thrown away now.
        backend.mutate { it.settings = it.settings.copy(syncScope = scope) }
    }

    override fun uploadPolicy(): Flow<UploadPolicy> = backend.observe { state, _ -> state.settings.uploadPolicy }

    override suspend fun setUploadPolicy(policy: UploadPolicy) {
        backend.gate(MockOperation.SyncControl)
        // Streaming exists to release local bytes quickly and staged defers exactly the upload
        // that release depends on, so the two are mutually exclusive rather than merely
        // discouraged.
        val streaming = backend.read { state -> state.imports.values.any { !it.isFinished } }
        if (streaming && policy == UploadPolicy.Staged) {
            throw LocalError.Storage("a streaming import is running; staged upload would defer its release")
        }
        backend.mutate { it.settings = it.settings.copy(uploadPolicy = policy) }
    }

    override suspend fun forceSyncNow() {
        backend.gate(MockOperation.SyncControl)
        if (backend.scenario.value.inboundState == InboundStateScenario.StaleRevival) {
            quarantineStaleRevival()
            throw UploadError.StaleRevival
        }
        backend.mutate { it.lastCompletedSync = backend.now() }
    }

    override suspend fun ensureRepresentation(
        id: AssetId,
        wanted: RepresentationTier,
    ): RepresentationTier {
        val asset = backend.requireAsset(id)
        // Already in hand: no request, and therefore no gate. A read path that reached the
        // network for something it already holds would be the bug, not the network.
        if (asset.heldRepresentations.holds(wanted)) return wanted
        backend.gate(MockOperation.RepresentationFetch)
        val held = asset.heldRepresentations
        val scenario = backend.scenario.value
        val pending =
            asset.syncState == SyncState.AwaitingOriginal ||
                scenario.originalFetch == OriginalFetchScenario.AwaitingUpload
        when {
            // Transient pending, explicitly distinct from a permanent loss: the UI shows an
            // "original still on device" badge and re-fetches when the feed flips.
            pending && wanted == RepresentationTier.Original -> throw BlobError.PendingUpload
            // `410 Gone`. Degrade down the ladder and keep the asset listed — a missing
            // derivative is a display problem, not a reason to make a photo disappear.
            scenario.originalFetch == OriginalFetchScenario.Gone -> return held.degradeFrom(wanted)
            // `403`. Surfaced as a revocation so the client re-syncs membership and capability
            // before degrading, rather than masking it as a missing file.
            scenario.originalFetch == OriginalFetchScenario.MembershipChanged -> throw AlbumError.NotAvailable
        }
        backend.mutate { it.put(asset.copy(heldRepresentations = HeldRepresentations(held.tiers + wanted))) }
        return wanted
    }

    /** Holds the replayed write for a human rather than applying or dropping it. */
    private fun quarantineStaleRevival() {
        val subject = backend.library.landmarks.quarantinedAssetId ?: return
        val at = backend.now()
        backend.mutate { state ->
            val asset = state.assets[subject] ?: return@mutate
            state.put(asset.copy(syncState = SyncState.Quarantined(QuarantineSurface.StaleRevival)))
            state.quarantine +=
                QuarantineEntry(
                    id = QuarantineId(DeterministicIds.uuidV7(at.toEpochMilli(), prng)),
                    surface = QuarantineSurface.StaleRevival.asWire(),
                    assetId = subject,
                    detectedAt = at,
                    detail = "A peer proposed a manifest naming a chain head this device has already moved past.",
                    code = ErrorCode("error.upload.stale_revival"),
                )
        }
    }
}
