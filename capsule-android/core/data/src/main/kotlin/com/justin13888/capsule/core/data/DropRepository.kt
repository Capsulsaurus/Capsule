package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DropId
import com.justin13888.capsule.core.model.PendingDrop
import kotlinx.coroutines.flow.Flow

/**
 * The inbox of files guests deposited through an upload link.
 *
 * Three operations, because a drop has three possible futures and no others: it sits in the
 * inbox, it becomes an asset, or it goes away. There is no state to set — [adopt] and
 * [discard] both remove the inbox row atomically with whatever they do instead, so a drop
 * cannot be half-adopted.
 */
interface DropRepository {
    /** Everything awaiting review. Surfaced as a quarantine-class "needs your attention". */
    fun inbox(): Flow<List<PendingDrop>>

    /**
     * Adopts a drop into a container album, minting an asset.
     *
     * The **adopter** is recorded as the asset's creator; the guest is never a signer, and
     * their descriptor was advisory throughout. Adoption reclassifies the already-stored
     * bytes in place rather than re-uploading them, so it costs no new bulk quota.
     */
    suspend fun adopt(
        dropId: DropId,
        into: AlbumId,
    ): AssetId

    /** Discards a drop, releasing its bytes on the next collection. */
    suspend fun discard(dropId: DropId)
}
