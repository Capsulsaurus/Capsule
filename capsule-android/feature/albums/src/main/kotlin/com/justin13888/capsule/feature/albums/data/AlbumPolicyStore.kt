package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.feature.albums.model.AlbumPolicy
import com.justin13888.capsule.feature.albums.model.HistoryPolicy
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.map
import java.time.Duration
import kotlin.math.absoluteValue

/**
 * Where an album's fixed policy is read from, and where a completed upgrade is recorded.
 *
 * A port rather than a repository method because album policy is MLS group metadata: it
 * arrives with the group's own state, not from the catalog, and the contract for that has
 * not been pinned down yet. Declaring the seam here means the screens are written against
 * the shape the design fixes — three read-only values plus a version to fork to — and the
 * day the real source lands, one binding changes.
 *
 * There is deliberately no setter for the three policy values. Changing them is an upgrade
 * ceremony, never an edit.
 */
interface AlbumPolicyStore {
    /** The album's policy, re-emitted when an upgrade lands it on a new version. */
    fun policy(albumId: AlbumId): Flow<AlbumPolicy>

    /** Records that [albumId] now runs [version], after a fork completed. */
    suspend fun recordUpgrade(
        albumId: AlbumId,
        version: String,
    )
}

/**
 * A policy store that keeps upgrades in memory and derives everything else from the album id.
 *
 * Deriving rather than randomising is the point: the same album shows the same pin on every
 * launch, so a screenshot golden and a hand-driven session agree, and an album that offers
 * an upgrade keeps offering it until the ceremony actually completes.
 */
class InMemoryAlbumPolicyStore(
    private val newestVersion: String = NEWEST_PROTOCOL_VERSION,
) : AlbumPolicyStore {
    private val upgraded = MutableStateFlow<Map<String, String>>(emptyMap())

    override fun policy(albumId: AlbumId): Flow<AlbumPolicy> =
        upgraded.map { versions ->
            val pin = versions[albumId.value] ?: derivedPin(albumId)
            AlbumPolicy(
                historyPolicy = derivedHistoryPolicy(albumId),
                protocolPin = pin,
                defaultRetention = DEFAULT_RETENTION,
                upgradeAvailableTo = newestVersion.takeIf { it != pin },
            )
        }

    override suspend fun recordUpgrade(
        albumId: AlbumId,
        version: String,
    ) {
        upgraded.value = upgraded.value + (albumId.value to version)
    }

    /** Older albums keep an older pin, so the upgrade path is reachable without a fixture. */
    private fun derivedPin(albumId: AlbumId): String =
        if (albumId.value.hashCode().absoluteValue % PIN_SPREAD == 0) newestVersion else OLDER_PROTOCOL_VERSION

    /** Full history is the default for a shared album; capped is the opt-in. */
    private fun derivedHistoryPolicy(albumId: AlbumId): HistoryPolicy =
        if (albumId.value.hashCode().absoluteValue % HISTORY_SPREAD == 0) {
            HistoryPolicy.Capped(CAPPED_HISTORY_WINDOW)
        } else {
            HistoryPolicy.Full
        }

    private companion object {
        /** The newest dated protocol version this build can fork an album to. */
        const val NEWEST_PROTOCOL_VERSION = "2026-06-01"

        /** What an album created before that version is pinned to. */
        const val OLDER_PROTOCOL_VERSION = "2026-01-01"

        /** One album in three is already current, so both policy states are reachable. */
        const val PIN_SPREAD = 3

        /** One album in four opts into capped history. */
        const val HISTORY_SPREAD = 4

        /** The design's default trash retention window. */
        val DEFAULT_RETENTION: Duration = Duration.ofDays(30)

        /** The capped-history window an album opts into. */
        val CAPPED_HISTORY_WINDOW: Duration = Duration.ofDays(90)
    }
}
