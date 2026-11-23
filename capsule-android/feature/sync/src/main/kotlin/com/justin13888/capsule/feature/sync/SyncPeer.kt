package com.justin13888.capsule.feature.sync

import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.model.DeviceId
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.combine
import java.time.Clock
import java.time.Duration
import java.time.Instant

/** One of the user's own devices, as the sync centre lists it. */
data class SyncPeer(
    /** The device's directory id. */
    val id: DeviceId,
    /** Its label, or `null` when the user never set one. */
    val displayName: String?,
    /** When this client last heard from it. */
    val lastSeen: Instant?,
    /**
     * Whether the last reconciliation with this device went over the local network.
     *
     * Worth stating in the UI because it changes what the user should expect: a LAN
     * reconciliation moves originals at wire speed and works with the server unreachable,
     * which is the whole reason peering exists.
     */
    val syncedOverLocalNetwork: Boolean,
)

/**
 * The user's other devices that this one can reach directly.
 *
 * A port because discovery is the one genuinely new mechanism peering introduces, and the
 * live mDNS responder is explicitly post-v1 — it is non-deterministic under CI. Keeping the
 * seam here means the sync centre renders real peering copy now and gains a real responder
 * later without changing.
 */
fun interface PeerDiscovery {
    /** Peers this device believes it can reach, re-emitted as the directory changes. */
    fun peers(): Flow<List<SyncPeer>>
}

/**
 * Peers derived from the signed device directory and the sessions bound to it.
 *
 * The directory *is* peering's trust anchor — a device not in it cannot complete the
 * handshake — so listing from it is the honest stand-in for a responder: it can never name a
 * device the channel would refuse. Recency stands in for "answered on this network", and is
 * deliberately conservative: a device nothing has been heard from is listed as merely
 * reachable rather than as having synced.
 */
class DirectoryPeerDiscovery(
    private val sessions: SessionRepository,
    private val clock: Clock,
) : PeerDiscovery {
    override fun peers(): Flow<List<SyncPeer>> =
        combine(sessions.devices(), sessions.sessions()) { devices, live ->
            val lastSeen =
                live
                    .mapNotNull { session -> session.deviceId?.let { it to session.lastActiveAt } }
                    .groupBy({ it.first }, { it.second })
                    .mapValues { (_, times) -> times.max() }
            devices
                .filter { it.isActive && !it.isThisDevice }
                .map { device ->
                    val seen = lastSeen[device.id]
                    SyncPeer(
                        id = device.id,
                        displayName = device.displayName,
                        lastSeen = seen,
                        syncedOverLocalNetwork = seen != null && Duration.between(seen, clock.instant()) < LAN_RECENCY,
                    )
                }
        }

    private companion object {
        /**
         * How recently a device must have been heard from to count as having synced locally.
         *
         * An hour, because peering is opportunistic: a device seen this morning and not since
         * is a device that may well be off the network now, and claiming a local sync that did
         * not happen would make the user trust a copy that does not exist.
         */
        val LAN_RECENCY: Duration = Duration.ofHours(1)
    }
}
