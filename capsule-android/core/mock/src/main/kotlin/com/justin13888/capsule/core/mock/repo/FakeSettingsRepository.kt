package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.SettingsRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.model.LibrarySettings
import com.justin13888.capsule.core.model.StaleSyncNotification
import kotlinx.coroutines.flow.Flow
import java.time.Duration

/**
 * The per-library preferences: one flow out, one field at a time in.
 *
 * The writes stay per-field rather than taking a whole record because a whole-record write is
 * a lost update waiting to happen — two screens each saving their own copy would silently undo
 * each other, and the settings screen is exactly where two screens are open at once.
 */
class FakeSettingsRepository(
    private val backend: MockBackend,
) : SettingsRepository {
    override fun settings(): Flow<LibrarySettings> = backend.observe { state, _ -> state.settings }

    override suspend fun setAutoSyncEnabled(enabled: Boolean) = write { it.copy(autoSyncEnabled = enabled) }

    override suspend fun setStaleSyncNotification(mode: StaleSyncNotification) =
        // Suppresses the warning and nothing else. Auto-sync is untouched, because a user who
        // silences a notification has not asked to stop backing up their photos.
        write { it.copy(staleSyncNotification = mode) }

    override suspend fun setDefaultRetention(window: Duration) {
        require(!window.isNegative && !window.isZero) { "a retention window must be positive" }
        // Applies to future deletes only. An asset already in trash keeps the deadline signed
        // with it, which is the entire point of signing it there.
        write { it.copy(defaultRetention = window) }
    }

    override suspend fun setRetainIdentifiersOnExport(retain: Boolean) {
        write { it.copy(retainIdentifiersOnExport = retain) }
    }

    override suspend fun setGatedViewGrace(window: Duration) = write { it.copy(gatedViewGrace = window) }

    private suspend fun write(transform: (LibrarySettings) -> LibrarySettings) {
        backend.gate(MockOperation.SettingsWrite)
        backend.mutate { it.settings = transform(it.settings) }
    }
}
