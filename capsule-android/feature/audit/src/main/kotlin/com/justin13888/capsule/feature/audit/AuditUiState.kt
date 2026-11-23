package com.justin13888.capsule.feature.audit

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.ErrorCode

/**
 * A failure the audit screen has to explain.
 *
 * Carries the stable `error.*` code rather than the exception, so the screen localizes a code
 * and never renders a transport status or an English detail string at the user. The code is
 * nullable because a purely local failure genuinely has none — inventing one would be a
 * catalogue key that means nothing.
 */
@Immutable
data class AuditFailure(
    /** The catalogue code to localize, or `null` for a device-local failure. */
    val code: ErrorCode?,
) {
    internal companion object {
        /** Wraps whatever the repositories threw, keeping only what the user can act on. */
        fun of(throwable: Throwable): AuditFailure = AuditFailure((throwable as? CapsuleError)?.code)
    }
}

/**
 * Everything the audit screens draw, as one value.
 *
 * The filtered list is derived here rather than in the composition so that "what is on
 * screen" and "what is selected" are decided in the same place. A screen that filtered while
 * rendering could show a detail pane for a row the list no longer contains, which is exactly
 * the state a two-pane layout makes visible.
 */
@Immutable
data class AuditUiState(
    /** Whether a reading is in flight and nothing has arrived yet. */
    val isLoading: Boolean = true,
    /** The failure to explain, or `null`. */
    val failure: AuditFailure? = null,
    /** The most recent complete reading. */
    val log: AuditLog = EMPTY_LOG,
    /** The signed device directory, for naming a record's signer. */
    val devices: Map<DeviceId, Device> = emptyMap(),
    /** What the user has narrowed the list to. */
    val filter: AuditFilter = AuditFilter(),
    /** The entry whose record the detail pane is showing. */
    val selectedId: String? = null,
) {
    /** The entries that survive [filter], newest first. */
    val visible: List<AuditEntry> = log.entries.filter(filter::accepts)

    /** The device this app is running on, when the directory names one. */
    val thisDevice: DeviceId? = devices.values.firstOrNull(Device::isThisDevice)?.id

    /** Nothing has ever been recorded — a new library rather than an over-narrow filter. */
    val isEmpty: Boolean get() = !isLoading && failure == null && log.entries.isEmpty()

    /** There is history, but the filter is hiding all of it. */
    val hasNoMatches: Boolean get() = !isLoading && failure == null && log.entries.isNotEmpty() && visible.isEmpty()

    /** The selected entry, or `null` when the selection no longer survives the filter. */
    val selected: AuditEntry? get() = visible.firstOrNull { it.id == selectedId }

    /** The entry with [id], if it is currently visible. */
    fun entry(id: String?): AuditEntry? = id?.let { key -> visible.firstOrNull { it.id == key } }

    private companion object {
        val EMPTY_LOG = AuditLog(entries = emptyList(), scannedAssets = 0, truncated = false)
    }
}
