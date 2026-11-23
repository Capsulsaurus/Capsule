package com.justin13888.capsule.feature.sync

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.UploadTier
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.toImmutableList
import java.time.Instant

/** Where one asset stands at one rung of the upload ladder. */
enum class TierProgress {
    /** Its session is queued but not open. */
    Queued,

    /** Its session is open and transferring. */
    Uploading,

    /** The last attempt failed with a code the user can act on. */
    Failed,
}

/** One asset's row in the upload queue. */
data class UploadQueueEntry(
    /** The asset. */
    val id: AssetId,
    /** When it was captured — what a row is headlined by, there being no stored file name. */
    val capturedAt: Instant,
    /** Which rung is outstanding. */
    val tier: UploadTier,
    /** Where that rung has got to. */
    val progress: TierProgress,
    /** Bytes the server has confirmed, when a session is open. */
    val bytesSent: Long = 0,
    /** The session's declared size, when a session is open. */
    val bytesTotal: Long = 0,
    /** The code behind a failure, or `null`. */
    val failure: ErrorCode? = null,
    /**
     * Whether this asset's local original is pinned until the rung completes.
     *
     * True for every outstanding original: no release path — cache eviction, a Move-import
     * source, a streaming release — may fire before the server confirms durability.
     */
    val pinsLocalOriginal: Boolean = false,
) {
    /** How far the open session has got, `0f..1f`. */
    val fraction: Float get() = if (bytesTotal <= 0) 0f else bytesSent.toFloat() / bytesTotal.toFloat()
}

/** One asset's row in the download and tiered-fetch queue. */
data class FetchQueueEntry(
    /** The asset. */
    val id: AssetId,
    /** When it was captured — what a row is headlined by, there being no stored file name. */
    val capturedAt: Instant,
    /** The rung the library's scope calls for. */
    val wanted: RepresentationTier,
    /** The best rung held locally right now. */
    val held: RepresentationTier,
    /**
     * Whether the wanted rung is permanently unavailable and the row has degraded.
     *
     * Distinct from merely outstanding: a degraded row is not waiting for anything and will
     * not resolve itself, so it earns the banner while an outstanding one does not.
     */
    val isDegraded: Boolean = false,
)

/** One tier of the staged upload ladder, and everything waiting at it. */
data class UploadTierGroup(
    /** Which rung. */
    val tier: UploadTier,
    /** The assets waiting at it. */
    val entries: ImmutableList<UploadQueueEntry>,
    /** Whether the current connection class permits opening this rung's sessions. */
    val opensOnThisConnection: Boolean,
) {
    /** How many assets are waiting. */
    val pending: Int get() = entries.size

    /** Whether anything is waiting at all. */
    val isClear: Boolean get() = entries.isEmpty()
}

/**
 * Turns a window of assets into the two queues the sync centre lists.
 *
 * Derived rather than stored, deliberately. The tier queue is re-derivable from server truth
 * — which roles the feed says are held, plus whatever sessions are in flight — and the design
 * says so explicitly: the local work queue is a rebuildable cache, never a second source of
 * truth. Deriving it here means the UI cannot show a queue the server has already drained.
 */
object TransferQueues {
    /**
     * Every rung still owed to the server, grouped by tier.
     *
     * All three tiers are always present, including the empty ones. A ladder that hid its
     * completed rungs would leave a user on a metered link unable to see that the index of
     * their library *has* escaped the device, which is the entire point of staging.
     */
    fun uploads(
        assets: List<Asset>,
        connection: ConnectionClass,
    ): ImmutableList<UploadTierGroup> {
        val rows = assets.mapNotNull(::uploadEntry)
        return UploadTier.entries
            .map { tier ->
                UploadTierGroup(
                    tier = tier,
                    entries = rows.filter { it.tier == tier }.toImmutableList(),
                    opensOnThisConnection = connection.rank() <= tier.opensAt.rank(),
                )
            }.toImmutableList()
    }

    /**
     * Every asset holding less than the library's scope calls for.
     *
     * The comparison is against the scope's own tier rather than against "the original",
     * because frugality is the contract: an asset that holds exactly what the user asked for
     * is finished, and listing it as outstanding would invite them to spend bandwidth they
     * deliberately chose not to.
     */
    fun fetches(
        assets: List<Asset>,
        scope: SyncScope,
    ): ImmutableList<FetchQueueEntry> =
        assets
            .filterNot { it.heldRepresentations.holds(scope.tier) }
            .map { asset ->
                FetchQueueEntry(
                    id = asset.id,
                    capturedAt = asset.captureTimestamp,
                    wanted = scope.tier,
                    held = asset.heldRepresentations.best,
                    isDegraded = asset.syncState == SyncState.AwaitingOriginal,
                )
            }.toImmutableList()

    private fun uploadEntry(asset: Asset): UploadQueueEntry? =
        when (val state = asset.syncState) {
            is SyncState.Queued ->
                UploadQueueEntry(
                    id = asset.id,
                    capturedAt = asset.captureTimestamp,
                    tier = state.nextTier,
                    progress = TierProgress.Queued,
                    pinsLocalOriginal = state.nextTier == UploadTier.Original,
                )

            is SyncState.Uploading ->
                UploadQueueEntry(
                    id = asset.id,
                    capturedAt = asset.captureTimestamp,
                    tier = state.tier,
                    progress = TierProgress.Uploading,
                    bytesSent = state.bytesSent,
                    bytesTotal = state.bytesTotal,
                    pinsLocalOriginal = state.tier == UploadTier.Original,
                )

            // Derived from the feed's `original_held`, never stored twice: everything but the
            // original has landed, so exactly one rung is outstanding.
            SyncState.AwaitingOriginal ->
                UploadQueueEntry(
                    id = asset.id,
                    capturedAt = asset.captureTimestamp,
                    tier = UploadTier.Original,
                    progress = TierProgress.Queued,
                    pinsLocalOriginal = true,
                )

            is SyncState.Failed ->
                UploadQueueEntry(
                    id = asset.id,
                    capturedAt = asset.captureTimestamp,
                    tier = UploadTier.Original,
                    progress = TierProgress.Failed,
                    failure = state.code,
                    pinsLocalOriginal = true,
                )

            SyncState.Synced, SyncState.LocalOnly, is SyncState.Quarantined -> null
        }

    /**
     * How weak a connection is, ordered so a gate is a comparison.
     *
     * [ConnectionClass] declares itself best-first but exposes no rank, and the tier ladder's
     * `opensAt` is a *floor* — "this rung opens on anything at least this good" — which needs
     * an ordering to evaluate.
     */
    private fun ConnectionClass.rank(): Int = ConnectionClass.entries.indexOf(this)
}
