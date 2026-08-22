package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.LibrarySettings
import com.justin13888.capsule.core.model.StaleSyncNotification
import kotlinx.coroutines.flow.Flow
import java.time.Duration

/**
 * The per-library preferences, read as one record and written one field at a time.
 *
 * One [settings] flow rather than a flow per knob: the settings screen shows them together,
 * and a dozen independent flows would have it recomposing a dozen times for one change. The
 * writes stay per-field because a whole-record write is a lost-update waiting to happen — two
 * screens each saving their own copy would silently undo each other.
 */
interface SettingsRepository {
    /** Every knob, re-emitted whenever one of them changes. */
    fun settings(): Flow<LibrarySettings>

    /** Turns auto-sync on or off. Off on platforms that cannot detect a metered link. */
    suspend fun setAutoSyncEnabled(enabled: Boolean)

    /** Sets what happens when the library falls behind. Never affects auto-sync itself. */
    suspend fun setStaleSyncNotification(mode: StaleSyncNotification)

    /**
     * Sets the retention window signed into new delete manifests.
     *
     * Applies to future deletes only. An asset already in trash keeps the deadline that was
     * signed with it, which is the point of signing it there.
     */
    suspend fun setDefaultRetention(window: Duration)

    /**
     * Sets whether the next export retains the identifiers the privacy table strips.
     *
     * A default for the next export prompt rather than a standing permission: sticky opt-in
     * is exactly the foot-gun where a user consents once and leaks a camera serial with every
     * share afterwards.
     */
    suspend fun setRetainIdentifiersOnExport(retain: Boolean)

    /** Sets how long a fresh-local-auth grant lasts, per gated view. */
    suspend fun setGatedViewGrace(window: Duration)
}
