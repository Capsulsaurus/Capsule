package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.AlbumId

/**
 * One contributor's own container album, seen as part of an aggregate.
 *
 * A constituent is admitted to the local view only because the viewer holds its keys *and*
 * it asserts the group id; [homeServer] is discovery information and nothing more. Modelling
 * it as advisory here mirrors that: nothing in this type is trusted to grant inclusion.
 *
 * @param albumId the constituent album itself.
 * @param homeServer where it is homed, for the per-origin degraded banner.
 * @param isOwn whether this is the viewer's own constituent — the only one they can remove
 *   photos from.
 * @param isReachable whether that origin answered on the last pull.
 * @param assetCount how many of its assets this device currently holds.
 */
data class FederatedConstituent(
    val albumId: AlbumId,
    val homeServer: String,
    val isOwn: Boolean,
    val isReachable: Boolean,
    val assetCount: Int,
)

/**
 * An aggregated federated album: N ordinary container albums presented as one.
 *
 * There is deliberately no shared mutable group object, so this type is a *rendering* of a
 * union of per-album assertions rather than a record of anything. Two consequences the UI
 * has to be honest about, and which are therefore expressed in the type: a contributor whose
 * origin is unreachable is [FederatedConstituent.isReachable] `false` and is still listed,
 * and only [ownConstituent] can have photos removed from it.
 *
 * @param groupId the client-minted UUIDv7 the creator shared in the invite.
 * @param name the group name, which converges by last-writer-wins across assertions.
 * @param constituents every album admitted into the local view.
 */
data class FederatedAlbum(
    val groupId: String,
    val name: String,
    val constituents: List<FederatedConstituent>,
) {
    /** The viewer's own contribution, or `null` when they are only reading. */
    val ownConstituent: FederatedConstituent? get() = constituents.firstOrNull(FederatedConstituent::isOwn)

    /** The origins that did not answer, in listing order — one banner each. */
    val unavailable: List<FederatedConstituent> get() = constituents.filterNot(FederatedConstituent::isReachable)

    /** How many assets the aggregate currently shows, across every reachable constituent. */
    val visibleAssetCount: Int get() = constituents.sumOf(FederatedConstituent::assetCount)
}
