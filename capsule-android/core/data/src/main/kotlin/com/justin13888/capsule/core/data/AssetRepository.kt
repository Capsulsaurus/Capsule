package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ContentHash
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.ProvenanceRecord
import kotlinx.coroutines.flow.Flow

/**
 * A single asset: how to find it, and the flags that are the asset's own state.
 *
 * The mutators here all write last-writer-wins registers, so each is one stamped write that
 * converges no matter what order it reaches another device in. None of them touches bytes:
 * rating, culling, and hiding are fully reversible, which is why the culling workflow can be
 * a single keystroke per asset without a confirmation.
 */
interface AssetRepository {
    /** Looks an asset up by its canonical id. Mirrors `Catalog.find_by_uuid`. */
    suspend fun findByUuid(id: AssetId): Asset?

    /**
     * Looks an asset up by its plaintext content hash. Mirrors `Catalog.find_by_hash`.
     *
     * This is import's deduplication check: a hit means the library already holds these exact
     * bytes and the candidate is a duplicate rather than a new asset.
     */
    suspend fun findByHash(hash: ContentHash): Asset?

    /** One asset, re-emitted as its metadata or lifecycle state changes. */
    fun observeAsset(id: AssetId): Flow<Asset?>

    /** Sets the numeric star rating, `0..5`. Orthogonal to the cull flag. */
    suspend fun setRating(
        id: AssetId,
        rating: Int,
    )

    /**
     * Sets the culling flag.
     *
     * Applied to a collapsed stack, this is the caller's decision to fan out: the design
     * flags each member individually, atomically staged, because a group has no stored flag
     * of its own.
     */
    suspend fun setCull(
        id: AssetId,
        flag: CullFlag,
    )

    /**
     * Hides or unhides an asset.
     *
     * View-layer only: the asset stays in its container, keeps syncing, and stays reachable
     * from its stack or an existing share. This is not deletion and not access control.
     */
    suspend fun setHidden(
        id: AssetId,
        hidden: Boolean,
    )

    /**
     * Writes or clears the stored GPS fix.
     *
     * The coordinates are stored **verbatim in the datum supplied** and never converted, so
     * the caller is responsible for having folded any BD-09 input at the input edge rather
     * than handing it over as a third datum.
     */
    suspend fun setGps(
        id: AssetId,
        fix: GpsFix?,
    )

    /**
     * The asset's provenance chain, oldest first.
     *
     * Append-only in both directions: a delete record survives a restore, so the chain is a
     * history rather than a current-state summary.
     */
    fun provenance(id: AssetId): Flow<List<ProvenanceRecord>>
}
