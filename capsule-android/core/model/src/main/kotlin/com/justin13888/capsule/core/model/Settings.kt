package com.justin13888.capsule.core.model

import java.time.Duration
import java.time.Instant

/**
 * How the client behaves when the library has fallen behind.
 *
 * Snoozing suppresses the warning and nothing else; disabling opts out of the warning
 * entirely and still does not touch auto-sync. Keeping the two distinct matters because a
 * user who silences a notification has not asked to stop backing up their photos, and a
 * client that conflated the two would quietly stop protecting them.
 */
sealed interface StaleSyncNotification {
    /** Warn once the library has been behind for [StaleSyncPolicy.STALE_AFTER]. */
    data object Enabled : StaleSyncNotification

    /** Suppressed until a date. The warning returns afterwards. */
    data class Snoozed(
        val until: Instant,
    ) : StaleSyncNotification

    /** Opted out. Auto-sync is unaffected. */
    data object Disabled : StaleSyncNotification
}

/** The thresholds behind [StaleSyncNotification]. */
object StaleSyncPolicy {
    /**
     * How long without a completed sync, while changes remain unsynced, before the user is
     * told.
     *
     * Two weeks is long enough that an ordinary week away does not nag, and short enough that
     * a device whose background windows the OS has stopped granting is caught before the
     * user has months of unbacked-up photos.
     */
    val STALE_AFTER: Duration = Duration.ofDays(14)

    /** The default snooze offered alongside the warning. */
    val DEFAULT_SNOOZE: Duration = Duration.ofDays(14)
}

/**
 * The per-library knobs a user can turn.
 *
 * Deliberately a small, flat record rather than a key-value bag: every field here changes
 * what the client does with the user's bytes, and a typed field is something the compiler
 * checks while a string key is something a typo silently disables.
 */
data class LibrarySettings(
    /** How much of each new asset to fetch by default. */
    val syncScope: SyncScope = SyncScope.MetadataAndThumbnails,
    /** Whether upload sessions open eagerly or in tier order. */
    val uploadPolicy: UploadPolicy = UploadPolicy.Full,
    /** Whether auto-sync runs at all. Platforms without metered detection leave it off. */
    val autoSyncEnabled: Boolean = true,
    /** What to do when the library falls behind. */
    val staleSyncNotification: StaleSyncNotification = StaleSyncNotification.Enabled,
    /** The retention window signed into a new delete manifest. */
    val defaultRetention: Duration = DeletionState.DEFAULT_RETENTION,
    /**
     * Whether the next export retains the identifiers the privacy table strips.
     *
     * Per-export rather than sticky, which is why it lives here as the *default* for the next
     * export prompt rather than as a standing permission: a user who opted in once and forgot
     * would leak a camera serial with every share thereafter.
     */
    val retainIdentifiersOnExport: Boolean = false,
    /** How long a fresh-local-auth grant lasts, per gated view. */
    val gatedViewGrace: Duration = LocalAuthGrant.DEFAULT_WINDOW,
)
