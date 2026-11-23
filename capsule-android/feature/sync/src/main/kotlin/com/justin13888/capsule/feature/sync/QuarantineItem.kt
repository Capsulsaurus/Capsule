package com.justin13888.capsule.feature.sync

import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.QuarantineSurface
import com.justin13888.capsule.core.model.SyncState
import java.time.Instant

/**
 * What the user can do about one held item.
 *
 * Every surface has exactly one, and none of them is "apply it". A quarantined item is never
 * silently dropped and never silently applied — the whole inventory exists so that a refusal
 * reaches a human — so the action is always a decision the user takes, not a repair the
 * client performs on their behalf.
 */
enum class QuarantineAction {
    /** Refuse the proposed change and stand on what this device already holds. */
    KeepMine,

    /** Open what arrived so the user can judge it before deciding. */
    Inspect,

    /** Reconcile two divergent histories — the only case with no safe default. */
    ChooseHistory,

    /** Route a deposit to the inbox that reviews deposits. */
    OpenInbox,

    /** Drop what arrived. Never offered where dropping could lose the user's own state. */
    Discard,
}

/** One item held for a human decision, with its reason and the one thing to do about it. */
data class QuarantineItem(
    /** The asset involved, when the refusal names one. */
    val assetId: AssetId?,
    /** Which refusal produced it, or `null` for a token this build does not know. */
    val surface: QuarantineSurface?,
    /** When it was held. */
    val detectedAt: Instant?,
    /** What the user can do about it. */
    val action: QuarantineAction,
) {
    /** A stable handle for the list, since not every held item names an asset. */
    val key: String get() = "${surface?.wire ?: UNKNOWN_KEY}:${assetId?.value ?: detectedAt}"

    private companion object {
        const val UNKNOWN_KEY = "unknown"
    }
}

/** The action one surface offers. */
fun QuarantineSurface?.defaultAction(): QuarantineAction =
    when (this) {
        QuarantineSurface.StaleRevival -> QuarantineAction.KeepMine
        QuarantineSurface.RestoreChainConflict -> QuarantineAction.ChooseHistory
        QuarantineSurface.PendingDropReview -> QuarantineAction.OpenInbox
        QuarantineSurface.VerifyAssetReject, QuarantineSurface.FederationSoftFail -> QuarantineAction.Discard
        QuarantineSurface.MalformedSidecar,
        QuarantineSurface.OrphanedOriginal,
        QuarantineSurface.AlbumUpgradeStranded,
        -> QuarantineAction.Inspect
        // An unrecognised surface is inspectable and nothing more: this build cannot know what
        // discarding it would cost, so it does not offer to.
        null -> QuarantineAction.Inspect
    }

/**
 * Everything currently held for a decision.
 *
 * A feature-owned port because `core:data` exposes no quarantine listing: the refusal is
 * visible on an asset's own [SyncState.Quarantined], and nowhere else. Naming the seam here
 * keeps the screen honest about where its rows come from, and leaves one obvious place for a
 * real listing to land.
 */
fun interface QuarantineSource {
    /** The held items, newest first. */
    suspend fun items(): List<QuarantineItem>
}

/**
 * Held items derived from the assets that carry a quarantined sync state.
 *
 * Scans a bounded window rather than the whole library: quarantine is meant to be small, and
 * a surface that had to walk a million assets to draw itself would be one nobody opened.
 */
class TimelineQuarantineSource(
    private val timeline: TimelineRepository,
    private val windowSize: Int = DEFAULT_WINDOW,
) : QuarantineSource {
    override suspend fun items(): List<QuarantineItem> =
        timeline
            .timelineWindow(offset = 0, limit = windowSize)
            .mapNotNull { asset ->
                val state = asset.syncState as? SyncState.Quarantined ?: return@mapNotNull null
                QuarantineItem(
                    assetId = asset.id,
                    surface = state.surface,
                    detectedAt = asset.importTimestamp,
                    action = state.surface.defaultAction(),
                )
            }

    private companion object {
        /** How far into the library to look. Generous for a surface that should hold nothing. */
        const val DEFAULT_WINDOW = 2_000
    }
}
