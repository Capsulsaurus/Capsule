package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.albums.model.FederatedAlbum
import com.justin13888.capsule.feature.albums.model.FederatedConstituent
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/**
 * The album-group assertions this device can see.
 *
 * A port because a group has **zero server surface** — it is a union of per-album assertions
 * carried in each contributor's own encrypted metadata stream, so there is nothing to fetch
 * and no endpoint to call. Until the metadata operation path lands there is no way to read a
 * real assertion, and inventing a repository method for it would be inventing a contract for
 * a transport that does not exist.
 */
interface FederatedAlbumSource {
    /** Every aggregate the viewer is admitted to, in a stable order. */
    fun groups(): Flow<List<FederatedAlbum>>
}

/**
 * Presents the viewer's shared containers as one aggregate.
 *
 * Faithful to the two rules that make the aggregation model safe. A constituent appears only
 * because the viewer holds its keys — here, because the album is one they are a member of —
 * so nothing a stranger asserts can inject itself. And exactly one constituent is the
 * viewer's own, because everyone writes their own and reads the others; that is what the
 * "you can only remove your own photos" copy on the screen is telling the truth about.
 *
 * The unreachable constituent is deliberate rather than incidental: the degraded per-origin
 * banner is a first-class state the design calls for, and a source that never produced one
 * would leave that state unreachable without a network fault.
 */
class DerivedFederatedAlbumSource(
    private val albums: AlbumRepository,
    private val peerServers: List<String> = DEFAULT_PEERS,
) : FederatedAlbumSource {
    override fun groups(): Flow<List<FederatedAlbum>> =
        albums.albums().map { all ->
            all
                .filter(::isSharedContainer)
                .map { album -> aggregate(album) }
        }

    /** Only a container with more than the owner in it can be part of an aggregate. */
    private fun isSharedContainer(album: Album): Boolean {
        val isContainer = album.kind.knownOrNull == AlbumKind.Container
        return isContainer && album.members.size > 1
    }

    private fun aggregate(album: Album): FederatedAlbum {
        val own =
            FederatedConstituent(
                albumId = album.id,
                homeServer = HOME_SERVER,
                isOwn = true,
                isReachable = true,
                assetCount = album.assetCount,
            )
        // One peer per non-owner member, and the last one is unreachable so the degraded
        // banner is always reachable from a running app.
        val peers =
            album.members.drop(1).mapIndexed { index, _ ->
                FederatedConstituent(
                    albumId = album.id,
                    homeServer = peerServers[index % peerServers.size],
                    isOwn = false,
                    isReachable = index < album.members.size - 2,
                    assetCount = album.assetCount / (index + 2),
                )
            }
        return FederatedAlbum(
            groupId = album.id.value,
            name = album.name,
            constituents = listOf(own) + peers,
        )
    }

    private companion object {
        /** What this account's own home server is called in the contributor list. */
        const val HOME_SERVER = "this server"

        /** The peers a shared album's other contributors are homed on. */
        val DEFAULT_PEERS = listOf("photos.example.org", "capsule.example.net")
    }
}
