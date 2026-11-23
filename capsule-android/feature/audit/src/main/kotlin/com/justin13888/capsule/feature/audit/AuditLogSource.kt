package com.justin13888.capsule.feature.audit

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.ModerationRepository
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.ModerationEvent
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first

/**
 * One reading of the log together with the directory needed to read it.
 *
 * The two travel as one value because a row that names a device id it cannot resolve is
 * worse than useless — the user is shown a UUID and left to guess whether it is their phone.
 * Emitting them together means the list never renders against a directory from a different
 * moment.
 */
@Immutable
data class AuditSnapshot(
    /** The assembled log. */
    val log: AuditLog,
    /** Every enrolled device, revoked ones included, keyed by id. */
    val devices: Map<DeviceId, Device>,
)

/**
 * Assembles the library audit log out of the repositories that own its two halves.
 *
 * There is no audit repository, and deliberately so: a stored audit table would be a *second*
 * account of what happened, free to disagree with the provenance chains and the moderation
 * log it claims to summarise. Deriving it means the screen can only ever show what the
 * evidence actually says.
 *
 * The cost of deriving it is that the chains are read per asset, which is why [scanLimit]
 * exists and why the result reports having been truncated. A million-asset library cannot
 * have its whole history assembled for a list, and quietly showing one page as though it were
 * everything would turn "I cannot find that delete" into "that delete never happened".
 */
class AuditLogSource(
    private val timeline: TimelineRepository,
    private val assets: AssetRepository,
    private val moderation: ModerationRepository,
    private val sessions: SessionRepository,
    /** How many assets' chains one reading covers. */
    private val scanLimit: Int = DEFAULT_SCAN_LIMIT,
) {
    /**
     * The log, re-assembled whenever the library, the moderation log or the directory moves.
     *
     * Keyed off [TimelineRepository.revision] rather than off any individual asset: the
     * revision is the one signal that says "something in the catalog changed", which is
     * exactly the granularity a whole-library summary needs.
     */
    fun snapshots(): Flow<AuditSnapshot> =
        combine(
            timeline.revision(),
            moderation.events(),
            sessions.devices(),
        ) { _, events, devices ->
            AuditSnapshot(log = assemble(events), devices = devices.associateBy(Device::id))
        }

    /**
     * Reads one page of history.
     *
     * Trash is scanned before the timeline, and that order is the point rather than a detail:
     * the entries a user most often comes here to check are deletes, and a deleted asset is
     * precisely the one the timeline no longer lists.
     */
    private suspend fun assemble(events: List<ModerationEvent>): AuditLog {
        val trashed = timeline.trash(0, scanLimit)
        val live = timeline.timelineWindow(0, scanLimit)
        val candidates = (trashed + live).distinctBy(Asset::id)
        val scanned = candidates.take(scanLimit)
        val lifecycle =
            scanned.flatMap { asset ->
                assets.provenance(asset.id).first().map(AuditEntry::Lifecycle)
            }
        val moderationEntries = events.map(AuditEntry::Moderation)
        return AuditLog(
            entries = (lifecycle + moderationEntries).sortedByDescending(AuditEntry::at),
            scannedAssets = scanned.size,
            // A full window is itself evidence of more: the repositories page, so a window
            // that came back full says nothing about what lies past it.
            truncated = candidates.size > scanned.size || live.size >= scanLimit,
        )
    }

    private companion object {
        /**
         * How many assets one reading covers.
         *
         * Sized for a screen rather than for a library: at roughly two records an asset this
         * is already a few hundred rows, more than anyone scrolls, and every asset past it
         * costs a chain read for history nobody is looking at.
         */
        const val DEFAULT_SCAN_LIMIT = 120
    }
}
