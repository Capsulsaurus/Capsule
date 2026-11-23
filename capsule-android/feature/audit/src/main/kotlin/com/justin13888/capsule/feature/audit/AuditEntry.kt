package com.justin13888.capsule.feature.audit

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.LifecycleAction
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.ModerationEventKind
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.knownOrNull
import java.time.Instant

/**
 * Where an entry in the log came from.
 *
 * Two origins and no more, because the log's whole claim is that these two are shown side by
 * side. A user who only ever saw their own writes would have no way to discover that an
 * operator acted on their library, which is precisely the silence the moderation design
 * forbids.
 */
enum class AuditCategory {
    /** A write to the user's own library, recorded in an asset's provenance chain. */
    Lifecycle,

    /** An administrative action taken against the account or one of its assets. */
    Moderation,
}

/**
 * One line of the library audit log.
 *
 * A sealed pair rather than a flattened row with nullable columns: the two origins carry
 * genuinely different evidence — a provenance record is signed by a device key and names the
 * chain head it followed, while a moderation event is the server's own statement — and a
 * single row type would invite the detail pane to render whichever half happened to be
 * populated.
 *
 * The underlying record is carried, not summarised. The screen exists so a user can read the
 * evidence behind an action, and a projection that dropped the hashes would leave them with
 * the app's word for it.
 */
@Immutable
sealed interface AuditEntry {
    /** Stable across reloads, so a selected entry survives the list being rebuilt. */
    val id: String

    /** When the action was recorded. The log's sort key, newest first. */
    val at: Instant

    /** Which half of the log this entry belongs to. */
    val category: AuditCategory

    /** The asset the action touched, absent for account-level moderation. */
    val assetId: AssetId?

    /** The device that signed the entry, absent when the server is the actor. */
    val signedBy: DeviceId?

    /** A write to the user's own library, with the signed record behind it. */
    @Immutable
    data class Lifecycle(
        /** The provenance record itself, verbatim. */
        val record: ProvenanceRecord,
    ) : AuditEntry {
        override val id: String get() = "lifecycle:${record.recordHash}"
        override val at: Instant get() = record.at
        override val category: AuditCategory get() = AuditCategory.Lifecycle
        override val assetId: AssetId get() = record.assetId
        override val signedBy: DeviceId get() = record.byDevice

        /** The action, when this build understands the token; `null` for a newer peer's. */
        val action: LifecycleAction? get() = record.action.knownOrNull
    }

    /** An operator action against this account, as the moderation log surfaced it. */
    @Immutable
    data class Moderation(
        /** The event itself, verbatim. */
        val event: ModerationEvent,
    ) : AuditEntry {
        override val id: String get() = "moderation:${event.id.value}"
        override val at: Instant get() = event.createdAt
        override val category: AuditCategory get() = AuditCategory.Moderation
        override val assetId: AssetId? get() = event.assetId

        /** Always absent: a moderation action is the server's statement, not a device's. */
        override val signedBy: DeviceId? get() = null

        /** The kind, when this build understands the token; `null` for a newer server's. */
        val kind: ModerationEventKind? get() = event.kind.knownOrNull
    }
}

/**
 * What the user has narrowed the log down to.
 *
 * Filters are expressed as *inclusions* rather than exclusions so that the empty-selection
 * case is unambiguous: no category selected means nothing matches, and the screen says so
 * with a way back, instead of silently reinterpreting it as "everything".
 */
@Immutable
data class AuditFilter(
    /** The origins to include. */
    val categories: Set<AuditCategory> = AuditCategory.entries.toSet(),
    /** When set, only records signed by this device are included. */
    val onlyDevice: DeviceId? = null,
) {
    /** Whether [entry] survives this filter. */
    fun accepts(entry: AuditEntry): Boolean {
        if (entry.category !in categories) return false
        val device = onlyDevice ?: return true
        return entry.signedBy == device
    }

    /** Whether the filter is letting everything through. */
    val isUnfiltered: Boolean
        get() = categories == AuditCategory.entries.toSet() && onlyDevice == null
}

/**
 * One assembled reading of the log.
 *
 * [truncated] is a first-class field rather than an inference from `scannedAssets`, because
 * the honest statement — "there is more history than this" — is one the screen must be able
 * to make without knowing the library's total size. Presenting a partial history as a
 * complete one is the failure mode that matters here: a user checking whether something was
 * deleted would read an absence as proof.
 */
@Immutable
data class AuditLog(
    /** Every entry gathered, newest first. */
    val entries: List<AuditEntry>,
    /** How many assets' chains were read to build [entries]. */
    val scannedAssets: Int,
    /** Whether the library holds more history than was read. */
    val truncated: Boolean,
)
