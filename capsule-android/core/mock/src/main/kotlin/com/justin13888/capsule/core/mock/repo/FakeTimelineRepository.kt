package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.TimelineIndex
import com.justin13888.capsule.core.data.TimelineIndexSource
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.DeletionState
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.flow.Flow
import java.time.Instant

/**
 * The library listing, the trash beneath it, and the geometry the gallery lays out from.
 *
 * Implements [TimelineIndexSource] as well as [TimelineRepository] because the two answer the
 * same question at two resolutions — "which assets" and "where do their tiles go" — and
 * splitting them across two objects over one mutable state would let the index drift a frame
 * behind the window it describes.
 */
class FakeTimelineRepository(
    private val backend: MockBackend,
) : TimelineRepository,
    TimelineIndexSource {
    override suspend fun timelineWindow(
        offset: Int,
        limit: Int,
    ): List<Asset> {
        backend.gate(MockOperation.TimelineRead)
        return window(backend.read(backend::visibleAssets), offset, limit)
    }

    override suspend fun timelineWindowFiltered(
        assetType: AssetType?,
        after: Instant?,
        before: Instant?,
        offset: Int,
        limit: Int,
    ): List<Asset> {
        backend.gate(MockOperation.TimelineRead)
        val filtered =
            backend.read(backend::visibleAssets).filter { asset ->
                (assetType == null || asset.assetType.knownOrNull == assetType) &&
                    (after == null || asset.captureTimestamp.isAfter(after)) &&
                    (before == null || asset.captureTimestamp.isBefore(before))
            }
        return window(filtered, offset, limit)
    }

    override fun revision(): Flow<Long> = backend.revisions()

    override suspend fun trash(
        offset: Int,
        limit: Int,
    ): List<Asset> {
        backend.gate(MockOperation.TimelineRead)
        if (!backend.isPopulated()) return emptyList()
        val trashed =
            backend
                .read { state -> state.assets.values.filter { it.deletion is DeletionState.Trashed } }
                .sortedByDescending { (it.deletion as DeletionState.Trashed).deletedAt }
        return window(trashed, offset, limit)
    }

    override suspend fun softDelete(
        id: AssetId,
        retentionUntil: Instant,
    ) {
        backend.gate(MockOperation.TrashWrite)
        val asset = backend.requireAsset(id)
        // The deadline is signed in here, at delete time, rather than looked up at purge time.
        // That is the guarantee: nothing downstream can shorten a window it did not sign.
        val deletion = DeletionState.Trashed(deletedAt = backend.now(), retentionUntil = retentionUntil)
        backend.mutate { it.put(asset.copy(deletion = deletion)) }
    }

    override suspend fun restore(id: AssetId) {
        backend.gate(MockOperation.TrashWrite)
        val asset = backend.requireAsset(id)
        backend.mutate { it.put(asset.copy(deletion = DeletionState.Live)) }
    }

    override suspend fun purge(id: AssetId) {
        backend.gate(MockOperation.TrashWrite)
        val asset = backend.requireAsset(id)
        val deletion = asset.deletion
        // Refused before the signed floor, and refused outright for a live asset: a purge is
        // the one operation here that cannot be undone, so it never runs on a guess.
        if (deletion !is DeletionState.Trashed) {
            throw LocalError.Storage("asset ${id.value} is not in the trash")
        }
        if (!deletion.isPurgeable(backend.now())) {
            throw LocalError.Storage("asset ${id.value} is inside its signed retention window")
        }
        backend.mutate { state ->
            state.assets.remove(id)
            state.provenance.remove(id)
        }
    }

    override suspend fun expiredTrash(asOf: Instant): List<Asset> {
        backend.gate(MockOperation.TimelineRead)
        if (!backend.isPopulated()) return emptyList()
        return backend.read { state ->
            state.assets.values.filter { (it.deletion as? DeletionState.Trashed)?.isPurgeable(asOf) == true }
        }
    }

    override fun timelineIndex(): Flow<TimelineIndex> = indexOver { true }

    override fun albumIndex(albumId: AlbumId): Flow<TimelineIndex> = indexOver { it.albumId == albumId }

    /**
     * A live index over the assets [predicate] admits.
     *
     * Both index flows go through here so the library-wide and per-album geometries can never
     * disagree about which assets a default view shows — two filters written separately would
     * eventually drift, and the symptom would be a scroll bar whose extent does not match the
     * grid under it.
     */
    private fun indexOver(predicate: (Asset) -> Boolean): Flow<TimelineIndex> =
        backend.observe { state, _ ->
            MockTimelineIndex.of(backend.visibleAssets(state).filter(predicate))
        }

    /**
     * A half-open window of [source], clamped rather than thrown on.
     *
     * A scrolled view reads windows against an index it may already have re-read past, so an
     * off-the-end request is an ordinary race and not a caller error. Clamping keeps the frame;
     * throwing would drop it.
     */
    private fun window(
        source: List<Asset>,
        offset: Int,
        limit: Int,
    ): List<Asset> {
        if (offset >= source.size || limit <= 0) return emptyList()
        return source.subList(offset.coerceAtLeast(0), (offset + limit).coerceAtMost(source.size))
    }
}
