package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import kotlinx.coroutines.flow.Flow
import java.time.Instant

/**
 * The main library listing and the trash beneath it.
 *
 * Reads are **paged and `suspend`**, not `Flow`s of the whole library: a timeline window is a
 * question about a specific range, and a library of a million assets cannot be emitted as a
 * list at all. What *is* observable is [revision] — a single signal that something changed —
 * which lets a scrolled view re-read only the windows it currently shows.
 *
 * The paging methods map one-to-one onto the pinned catalog surface (`query_timeline`,
 * `query_timeline_filtered`, `query_trash`, `restore_asset`, `purge_asset`), so a real
 * binding fills them in without reshaping anything.
 */
interface TimelineRepository {
    /**
     * A window of the timeline, newest first, excluding hidden, trashed, and stack-suppressed
     * assets.
     *
     * Mirrors `Catalog.query_timeline(offset, limit)`.
     */
    suspend fun timelineWindow(
        offset: Int,
        limit: Int,
    ): List<Asset>

    /**
     * A window filtered by type and capture-time range. Any filter left `null` is not applied.
     *
     * Mirrors `Catalog.query_timeline_filtered`. [assetType] is typed rather than a raw token
     * because an unknown type cannot match anything the catalog holds — there is nothing to
     * preserve, so refusing it at the call site is better than passing it through.
     */
    suspend fun timelineWindowFiltered(
        assetType: AssetType?,
        after: Instant?,
        before: Instant?,
        offset: Int,
        limit: Int,
    ): List<Asset>

    /**
     * Increments whenever the timeline's contents or ordering change.
     *
     * An opaque counter rather than a change description: consumers re-read the windows they
     * are showing, and a fine-grained diff would be a second source of truth about the
     * catalog's state that could drift from the catalog itself.
     */
    fun revision(): Flow<Long>

    /**
     * A window of soft-deleted assets, most recently deleted first.
     *
     * Mirrors `Catalog.query_trash`. Gated behind fresh local auth at the UI layer.
     */
    suspend fun trash(
        offset: Int,
        limit: Int,
    ): List<Asset>

    /**
     * Soft-deletes an asset, signing [retentionUntil] into the delete manifest.
     *
     * The deadline is a parameter rather than a server-side policy lookup because that is the
     * guarantee: the floor on the purge travels with the deletion, so nothing downstream can
     * shorten it.
     */
    suspend fun softDelete(
        id: AssetId,
        retentionUntil: Instant,
    )

    /**
     * Recovers a soft-deleted asset within its retention window.
     *
     * Mirrors `Catalog.restore_asset`. Appends a restore record; the original delete record
     * stays in the chain, so "deleted then restored" remains legible.
     */
    suspend fun restore(id: AssetId)

    /**
     * Permanently removes an asset.
     *
     * Mirrors `Catalog.purge_asset`. Refused before the asset's signed `retention_until`.
     */
    suspend fun purge(id: AssetId)

    /** Trashed assets whose retention window has elapsed — the purge worker's queue. */
    suspend fun expiredTrash(asOf: Instant): List<Asset>
}
