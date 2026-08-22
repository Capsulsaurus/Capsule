package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.ResolvedDefaultAlbum
import kotlinx.coroutines.flow.Flow

/**
 * Container albums, their membership, and the default-album pointer.
 *
 * [albums] is a `Flow` while [albumAssets] is paged and `suspend`: a user has tens of albums
 * and can watch the whole list change, but an album can hold a million assets and cannot.
 *
 * Views are not created or deleted here. A view holds no keys, owns no assets, and is
 * computed client-side, so it has no lifecycle to manage — its "creation" is a smart-album
 * definition, which lives on [SearchRepository].
 */
interface AlbumRepository {
    /** Every album the viewer can see, container and view alike. */
    fun albums(): Flow<List<Album>>

    /** One album, re-emitted as its name, cover, or membership changes. */
    fun album(id: AlbumId): Flow<Album?>

    /**
     * A window of an album's assets.
     *
     * Mirrors `Catalog.query_album_assets(album_id, offset, limit)`.
     */
    suspend fun albumAssets(
        albumId: AlbumId,
        offset: Int,
        limit: Int,
    ): List<Asset>

    /** Creates a container album. Mirrors `Catalog.insert_album`. */
    suspend fun insertAlbum(album: Album)

    /** Updates a container album's name or cover. Mirrors `Catalog.update_album`. */
    suspend fun updateAlbum(album: Album)

    /**
     * Deletes a container album. Mirrors `Catalog.delete_album`.
     *
     * Refused while the album is the designated default — import must always have a home, so
     * the user repoints first.
     */
    suspend fun deleteAlbum(id: AlbumId)

    /**
     * Moves an asset into a container, or clears its membership.
     *
     * Mirrors `Catalog.set_asset_album`. Idempotent: replaying a move finds the target state
     * already in place and does nothing.
     */
    suspend fun setAssetAlbum(
        assetId: AssetId,
        albumId: AlbumId?,
    )

    /**
     * Resolves where an import should land, and records which rule decided.
     *
     * Always a container, never a view. [scope] narrows the resolution to a specific source
     * when the caller has one; without it the owner pointer and then the derived de facto
     * album decide.
     */
    suspend fun resolveDefaultAlbum(scope: ImportScope? = null): ResolvedDefaultAlbum

    /** Repoints the owner's default album. */
    suspend fun setDefaultAlbum(albumId: AlbumId)
}
