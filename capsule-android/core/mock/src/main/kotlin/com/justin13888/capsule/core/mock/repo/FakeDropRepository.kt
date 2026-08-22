package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.DropError
import com.justin13888.capsule.core.data.DropRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.seed.DeterministicIds
import com.justin13888.capsule.core.mock.seed.Prng
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.ContentHash
import com.justin13888.capsule.core.model.ContentType
import com.justin13888.capsule.core.model.Dimensions
import com.justin13888.capsule.core.model.DropId
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.MediaKind
import com.justin13888.capsule.core.model.PendingDrop
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.flow.Flow

/**
 * The inbox of files guests deposited through an upload link.
 *
 * Three operations because a drop has three futures and no others, and no state to set: adopt
 * and discard each remove the inbox row in the same step that creates the asset or releases
 * the bytes, so a drop can never be observed half-adopted.
 */
class FakeDropRepository(
    private val backend: MockBackend,
) : DropRepository {
    private val prng = Prng(Prng.mixSeed(backend.library.spec.seed, "drops"))

    override fun inbox(): Flow<List<PendingDrop>> =
        backend.observe { state, scenario ->
            if (scenario.populated) state.drops.values.toList() else emptyList()
        }

    override suspend fun adopt(
        dropId: DropId,
        into: AlbumId,
    ): AssetId {
        backend.gate(MockOperation.DropWrite)
        val drop = backend.read { it.drops[dropId] } ?: throw DropError.NotInInbox
        backend.requireAlbum(into)
        val asset = mintAsset(drop, into)
        backend.mutate { state ->
            state.drops.remove(dropId)
            state.insertOrdered(asset)
        }
        return asset.id
    }

    override suspend fun discard(dropId: DropId) {
        backend.gate(MockOperation.DropWrite)
        backend.mutate { state ->
            state.drops.remove(dropId) ?: throw DropError.NotInInbox
        }
    }

    /**
     * Mints the asset an adoption creates.
     *
     * The **adopter** is recorded as the importing device and the guest is never a signer:
     * their descriptor was advisory throughout, which is why the declared filename does not
     * become a path and the declared content type is re-stated through the closed enum here
     * rather than trusted as it arrived.
     */
    private fun mintAsset(
        drop: PendingDrop,
        into: AlbumId,
    ): Asset {
        val now = backend.now()
        val contentType = drop.descriptor.contentType.knownOrNull ?: ContentType.Jpeg
        val isVideo = contentType.mediaKind == MediaKind.Video
        return Asset(
            id = AssetId(DeterministicIds.uuidV7(now.toEpochMilli(), prng)),
            assetType = (if (isVideo) AssetType.Video else AssetType.Photo).asWire(),
            captureTimestamp = drop.receivedAt,
            captureUtc = drop.receivedAt,
            captureTzSource =
                com.justin13888.capsule.core.model.CaptureTzSource.Floating
                    .asWire(),
            importTimestamp = now,
            contentHash = ContentHash(drop.descriptor.ciphertextHash.value),
            dimensions = Dimensions(ADOPTED_WIDTH, ADOPTED_HEIGHT),
            duration = null,
            albumId = into,
            rating = 0,
            isHidden = false,
            contentType = contentType.asWire(),
            importedByDevice = backend.library.landmarks.thisDeviceId,
            heldRepresentations = HeldRepresentations(setOf(RepresentationTier.Lqip, RepresentationTier.Thumbnail)),
            // Adoption reclassifies bytes already stored rather than re-uploading them, so the
            // asset is durable from the moment it exists.
            syncState = SyncState.Synced,
        )
    }

    private companion object {
        const val ADOPTED_WIDTH = 4032
        const val ADOPTED_HEIGHT = 3024
    }
}
