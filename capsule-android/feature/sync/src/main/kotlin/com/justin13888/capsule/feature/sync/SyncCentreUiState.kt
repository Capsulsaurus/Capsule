package com.justin13888.capsule.feature.sync

import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.StaleSyncNotification
import com.justin13888.capsule.core.model.StaleSyncPolicy
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import java.time.Duration
import java.time.Instant

/**
 * Everything the sync centre draws.
 *
 * [now] is carried rather than read at render time so the staleness decision is a property of
 * the state a test can assert, not of whenever the composition happened to run.
 */
data class SyncCentreUiState(
    /** The first read is still in flight. */
    val isLoading: Boolean = true,
    /** The device's current path quality. */
    val connection: ConnectionClass = ConnectionClass.Unmetered,
    /** When a reconciliation last completed, or `null` if none ever has. */
    val lastCompletedSync: Instant? = null,
    /** The instant staleness is measured against. */
    val now: Instant = Instant.EPOCH,
    /** How many rungs are still owed to the server. */
    val pendingUploads: Int = 0,
    /** How many items are held for a decision. */
    val quarantined: Int = 0,
    /** How much of each new asset this library fetches. */
    val scope: SyncScope = SyncScope.MetadataAndThumbnails,
    /** Whether upload sessions open eagerly or in tier order. */
    val uploadPolicy: UploadPolicy = UploadPolicy.Full,
    /** What the client does when the library falls behind. */
    val staleNotification: StaleSyncNotification = StaleSyncNotification.Enabled,
    /** The user's own devices this one can reach directly. */
    val peers: ImmutableList<SyncPeer> = persistentListOf(),
    /** A user-initiated reconciliation is running. */
    val isSyncing: Boolean = false,
    /** The coded failure to localize, or `null`. */
    val failure: ErrorCode? = null,
    /** Set when a failure carries no catalogue code — a purely local refusal. */
    val hasLocalFailure: Boolean = false,
) {
    /** How long the library has been behind, or `null` when nothing has ever synced. */
    val behindFor: Duration? get() = lastCompletedSync?.let { Duration.between(it, now) }

    /**
     * Whether the library has been behind past the warning threshold with work outstanding.
     *
     * Both halves matter. A device that has synced nothing because there is nothing to sync is
     * not behind, and warning it would train the user to dismiss the one warning that counts.
     */
    val isBehindThreshold: Boolean
        get() {
            if (pendingUploads <= 0) return false
            val behind = behindFor ?: return true
            return behind >= StaleSyncPolicy.STALE_AFTER
        }

    /** Whether the staleness warning is currently suppressed until a date. */
    val snoozedUntil: Instant?
        get() = (staleNotification as? StaleSyncNotification.Snoozed)?.until?.takeIf { it.isAfter(now) }

    /** Whether the user opted out of the warning. Auto-sync is unaffected either way. */
    val isStaleWarningDisabled: Boolean get() = staleNotification == StaleSyncNotification.Disabled

    /** Whether the staleness banner should be shown right now. */
    val showsStaleWarning: Boolean
        get() = isBehindThreshold && !isStaleWarningDisabled && snoozedUntil == null

    /** How many whole days the library has been behind, for the banner's copy. */
    val behindDays: Long get() = behindFor?.toDays() ?: StaleSyncPolicy.STALE_AFTER.toDays()

    /** Whether there is no usable path at all. */
    val isOffline: Boolean get() = connection == ConnectionClass.Offline

    /**
     * Whether an error state should replace the content.
     *
     * Only before the first successful read. A failure after that — a refused force-sync, say —
     * is a banner over content that is still true, because throwing the whole screen away
     * would hide the sync state the user came here to look at.
     */
    val hasFailed: Boolean get() = isLoading && (failure != null || hasLocalFailure)
}
