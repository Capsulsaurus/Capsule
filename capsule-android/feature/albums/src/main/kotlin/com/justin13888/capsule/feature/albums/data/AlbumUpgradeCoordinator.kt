package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.feature.albums.model.UpgradeCeremony
import com.justin13888.capsule.feature.albums.model.UpgradePhase
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.map
import java.time.Clock
import java.time.Instant

/**
 * Drives one album's tombstone-plus-fork upgrade.
 *
 * A port because every step is really an MLS commit or a server poll, and none of that
 * exists yet. What *does* exist and must not be re-derived later is the ordering, which
 * lives in [UpgradeCeremony]; this interface is only the place the ordering meets the
 * outside world, so the default implementation is a faithful driver rather than a stub that
 * jumps to the end.
 *
 * The phase is observable rather than returned because the ceremony outlives the screen: a
 * client that crashed mid-way reads its phase back on restart and resumes from there, which
 * is only possible if the phase is state rather than the result of a call.
 */
interface AlbumUpgradeCoordinator {
    /** Where [albumId]'s upgrade has got to. */
    fun phase(albumId: AlbumId): Flow<UpgradePhase>

    /**
     * Issues the freeze proposal.
     *
     * Returns `false` when another intent is already in flight for this album — the refusal
     * that stops two proposals producing two divergent forks.
     */
    suspend fun propose(albumId: AlbumId): Boolean

    /** Takes the next step: drain, tombstone, fork, or replay. */
    suspend fun advance(albumId: AlbumId)

    /** Abandons the ceremony; the album returns to its old version and queued writes flush. */
    suspend fun abort(albumId: AlbumId)
}

/**
 * An in-memory coordinator that walks the real phase order.
 *
 * The two things it simulates are the two the design says the *server* answers: how many
 * upload sessions are still in flight, and whether every member recomputed the same frozen
 * state hash. Both are constructor parameters so a test can put the ceremony into its
 * failure paths without waiting on anything.
 */
class InMemoryAlbumUpgradeCoordinator(
    private val clock: Clock = Clock.systemUTC(),
    private val toVersion: String = "2026-06-01",
    private val inFlightUploads: (AlbumId) -> Int = { 0 },
    private val membersAgree: (AlbumId) -> Boolean = { true },
) : AlbumUpgradeCoordinator {
    private val phases = MutableStateFlow<Map<String, UpgradePhase>>(emptyMap())

    override fun phase(albumId: AlbumId): Flow<UpgradePhase> = phases.map { it[albumId.value] ?: UpgradePhase.Idle }

    override suspend fun propose(albumId: AlbumId): Boolean {
        val current = currentPhase(albumId)
        val next =
            UpgradeCeremony.propose(
                current = current,
                intentId = "upgrade-${albumId.value}-${clock.millis()}",
                now = now(),
            ) ?: return false
        put(albumId, next)
        return true
    }

    override suspend fun advance(albumId: AlbumId) {
        val next =
            UpgradeCeremony.advance(
                current = currentPhase(albumId),
                inFlightUploads = inFlightUploads(albumId),
                frozenStateHash = frozenStateHash(albumId),
                membersAgree = membersAgree(albumId),
                toVersion = toVersion,
                now = now(),
            )
        put(albumId, next)
    }

    override suspend fun abort(albumId: AlbumId) {
        put(albumId, UpgradeCeremony.abort(currentPhase(albumId)))
    }

    private fun currentPhase(albumId: AlbumId): UpgradePhase = phases.value[albumId.value] ?: UpgradePhase.Idle

    private fun put(
        albumId: AlbumId,
        phase: UpgradePhase,
    ) {
        phases.value = phases.value + (albumId.value to phase)
    }

    private fun now(): Instant = clock.instant()

    /**
     * A stand-in for the hash over the album's canonical frozen state.
     *
     * Deterministic per album, because the property the ceremony turns on is that every
     * member computes the *same* value — a random one would make disagreement the default.
     */
    private fun frozenStateHash(albumId: AlbumId): String = "frozen-${albumId.value.hashCode().toUInt()}"
}
