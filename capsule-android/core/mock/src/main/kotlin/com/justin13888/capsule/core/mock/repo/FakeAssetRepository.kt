package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ContentHash
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.LifecycleAction
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.asWire
import kotlinx.coroutines.flow.Flow

/**
 * A single asset and the flags that are its own state.
 *
 * Each mutator writes through [MockBackend.mutate], so the flow a viewer is collecting
 * re-emits before the call returns. That is not a convenience: the culling workflow is one
 * keystroke per asset with no confirmation, and a UI that had to poll for its own write would
 * show the previous frame's flag on every keypress.
 */
class FakeAssetRepository(
    private val backend: MockBackend,
) : AssetRepository {
    override suspend fun findByUuid(id: AssetId): Asset? {
        backend.gate(MockOperation.AssetRead)
        return backend.read { it.assets[id] }.takeIf { backend.isPopulated() }
    }

    override suspend fun findByHash(hash: ContentHash): Asset? {
        backend.gate(MockOperation.AssetRead)
        if (!backend.isPopulated()) return null
        return backend.read { state -> state.assets.values.firstOrNull { it.contentHash == hash } }
    }

    override fun observeAsset(id: AssetId): Flow<Asset?> =
        backend.observe { state, scenario -> if (scenario.populated) state.assets[id] else null }

    override suspend fun setRating(
        id: AssetId,
        rating: Int,
    ) {
        require(rating in RATING_RANGE) { "rating must be in $RATING_RANGE, was $rating" }
        writeFlags(id) { it.copy(rating = rating) }
    }

    override suspend fun setCull(
        id: AssetId,
        flag: CullFlag,
    ) = writeFlags(id) { it.copy(cull = flag.asWire()) }

    override suspend fun setHidden(
        id: AssetId,
        hidden: Boolean,
    ) = writeFlags(id) { it.copy(isHidden = hidden) }

    override suspend fun setGps(
        id: AssetId,
        fix: GpsFix?,
    ) {
        // Stored verbatim in whatever datum the caller supplied. Folding a BD-09 input happens
        // at the input edge, not here: converting at rest has no exact inverse and would
        // destroy the user's ground truth.
        writeFlags(id) { it.copy(gps = fix) }
    }

    override fun provenance(id: AssetId): Flow<List<ProvenanceRecord>> =
        backend.observe { state, _ ->
            state.provenance[id] ?: state.assets[id]?.let(::createRecordOnly) ?: emptyList()
        }

    /**
     * Applies one last-writer-wins register write.
     *
     * The forward-schema check is here rather than at each call site because it applies to all
     * four mutators identically: none of them can re-sign a sidecar whose tokens this build
     * does not understand.
     */
    private suspend fun writeFlags(
        id: AssetId,
        transform: (Asset) -> Asset,
    ) {
        backend.gate(MockOperation.AssetWrite)
        val asset = backend.requireAsset(id)
        backend.requireWritable(asset)
        backend.mutate { it.put(transform(asset)) }
    }

    /**
     * The one-record chain an asset with no recorded history still has.
     *
     * Every asset has a `create`, so returning an empty list for one would be a lie the UI
     * would render as "no provenance" rather than as "created and never touched".
     */
    private fun createRecordOnly(asset: Asset): List<ProvenanceRecord> =
        listOf(
            ProvenanceRecord(
                assetId = asset.id,
                action = LifecycleAction.Create.asWire(),
                priorProvenanceHash = null,
                recordHash = asset.contentHash.value,
                byDevice = asset.importedByDevice ?: backend.library.landmarks.thisDeviceId,
                at = asset.importTimestamp,
            ),
        )

    private companion object {
        val RATING_RANGE = 0..5
    }
}
