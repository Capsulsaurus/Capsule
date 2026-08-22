package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.AlbumError
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DefaultAlbumRule
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.ResolvedDefaultAlbum
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.flow.Flow

/**
 * Container albums, their contents, and the default-album pointer.
 *
 * The default album cannot be deleted here, and the refusal is structural rather than polite:
 * import always needs a home, so a client that deleted the designated default would leave the
 * next import with nowhere to land.
 */
class FakeAlbumRepository(
    private val backend: MockBackend,
) : AlbumRepository {
    override fun albums(): Flow<List<Album>> =
        backend.observe { state, scenario ->
            if (scenario.populated) {
                state.albums.map { it.copy(isDefault = it.id == state.defaultAlbumId) }
            } else {
                // A first-run library still has its default container: the pointer exists
                // before anything has been filed into it.
                state.albums.filter { it.id == state.defaultAlbumId }.map { it.copy(assetCount = 0) }
            }
        }

    override fun album(id: AlbumId): Flow<Album?> =
        backend.observe { state, _ ->
            state.albums.firstOrNull { it.id == id }?.copy(isDefault = id == state.defaultAlbumId)
        }

    override suspend fun albumAssets(
        albumId: AlbumId,
        offset: Int,
        limit: Int,
    ): List<Asset> {
        backend.gate(MockOperation.AlbumRead)
        val members = backend.read { state -> backend.visibleAssets(state).filter { it.albumId == albumId } }
        if (offset >= members.size || limit <= 0) return emptyList()
        return members.subList(offset.coerceAtLeast(0), (offset + limit).coerceAtMost(members.size))
    }

    override suspend fun insertAlbum(album: Album) {
        backend.gate(MockOperation.AlbumWrite)
        if (album.kind.knownOrNull != AlbumKind.Container) throw AlbumError.InvalidId
        backend.mutate { it.albums += album }
    }

    override suspend fun updateAlbum(album: Album) {
        backend.gate(MockOperation.AlbumWrite)
        backend.mutate { state ->
            val at = state.albums.indexOfFirst { it.id == album.id }
            if (at < 0) throw AlbumError.NotAvailable
            state.albums[at] = album
        }
    }

    override suspend fun deleteAlbum(id: AlbumId) {
        backend.gate(MockOperation.AlbumWrite)
        val album = backend.requireAlbum(id)
        if (id == backend.read { it.defaultAlbumId } || !album.isDeletable) throw AlbumError.NotAvailable
        backend.mutate { state ->
            state.albums.removeAll { it.id == id }
            state.assets.values
                .filter { it.albumId == id }
                .forEach { state.put(it.copy(albumId = state.defaultAlbumId)) }
        }
    }

    override suspend fun setAssetAlbum(
        assetId: AssetId,
        albumId: AlbumId?,
    ) {
        backend.gate(MockOperation.AlbumWrite)
        val asset = backend.requireAsset(assetId)
        // A view holds no keys and owns nothing, so it can never be a destination. Refusing
        // here rather than accepting and quietly doing nothing is what makes the mistake
        // visible while it is still a bug in the caller.
        if (albumId != null) {
            val target = backend.requireAlbum(albumId)
            if (target.kind.knownOrNull != AlbumKind.Container) throw AlbumError.NotAvailable
        }
        // Idempotent: replaying a move finds the target state already in place.
        if (asset.albumId == albumId) return
        backend.mutate { it.put(asset.copy(albumId = albumId)) }
    }

    override suspend fun resolveDefaultAlbum(scope: ImportScope?): ResolvedDefaultAlbum {
        backend.gate(MockOperation.AlbumRead)
        // The resolution order the design fixes, first match wins. Scope overrides and
        // per-source-kind defaults are deferred post-v1, so a scope with a recorded
        // destination resolves through the override rule and everything else falls to the
        // owner's pointer.
        val override = scope?.locator?.let { locator -> backend.read { it.scopeDestinations[locator] } }
        return if (override != null) {
            ResolvedDefaultAlbum(albumId = override, rule = DefaultAlbumRule.ScopeOverride)
        } else {
            ResolvedDefaultAlbum(albumId = backend.read { it.defaultAlbumId }, rule = DefaultAlbumRule.OwnerPointer)
        }
    }

    override suspend fun setDefaultAlbum(albumId: AlbumId) {
        backend.gate(MockOperation.AlbumWrite)
        val target = backend.requireAlbum(albumId)
        if (target.kind.knownOrNull != AlbumKind.Container) throw AlbumError.NotAvailable
        backend.mutate { it.defaultAlbumId = albumId }
    }
}
