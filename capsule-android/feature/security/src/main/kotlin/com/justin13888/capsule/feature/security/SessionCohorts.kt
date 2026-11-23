package com.justin13888.capsule.feature.security

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.DeviceCohort
import com.justin13888.capsule.core.model.Session
import java.time.Instant

/**
 * The sessions one physical device appears to have opened.
 *
 * Grouping exists because reinstalling the app re-enrolls with a new `device_id` by design, so
 * one phone accumulates several ledger rows over its life and a flat list is unreadable. The
 * grouping key is client-asserted and unverifiable, which is why nothing here is ever an input
 * to a decision: it decides what a row *says*, never what a session is *allowed to do*.
 */
@Immutable
data class SessionCohortGroup(
    /** The cohort hash, or `null` for sessions that asserted none. */
    val cohortHash: String?,
    /** When the durable cohort map first saw this device, if it carries a row for it. */
    val firstSeen: Instant?,
    /** When it last saw it. */
    val lastSeen: Instant?,
    /** The sessions under it, most recently active first. */
    val sessions: List<Session>,
) {
    /** Whether the session this process is using is in this group. */
    val isCurrent: Boolean = sessions.any(Session::isCurrent)

    /**
     * Whether the group may be described as a device the user has used before.
     *
     * True for any grouped cohort that is not this device. The cohort map is durable and
     * outlives the sessions under it, so a row existing at all *is* the statement that this
     * hardware has been here before — which is exactly what the copy asserts, and exactly as
     * much as it is entitled to assert.
     */
    val seenBefore: Boolean = !isCurrent && cohortHash != null

    /** How many sessions are grouped here, for a count the row can show. */
    val sessionCount: Int get() = sessions.size
}

/**
 * Turns a flat session ledger into something a person can read.
 *
 * Pure, so the grouping the screen shows is assertable without a repository or a composition.
 */
object SessionCohorts {
    /**
     * Groups [sessions] by the cohort each asserted, enriched from the durable [cohorts] map.
     *
     * Ordering is deliberate rather than incidental: this device first — it is the row the
     * user is checking themselves against — then the rest by recency, then whatever asserted
     * no cohort at all. Ungrouped sessions go last because "we do not know which device this
     * is" is the least useful thing on the screen, not the most alarming.
     *
     * A cohort with no row in the durable map still groups. The map is a convenience for
     * first-seen and last-seen dates, and a session whose hash is not in it is not thereby
     * suspicious — it is a session whose map row has not been written or has been pruned.
     */
    fun group(
        sessions: List<Session>,
        cohorts: List<DeviceCohort>,
    ): List<SessionCohortGroup> {
        val byHash = cohorts.associateBy(DeviceCohort::cohortHash)
        val groups =
            sessions
                .groupBy { it.cohortHash?.takeIf(String::isNotBlank) }
                .map { (hash, members) -> groupOf(hash, byHash[hash], members) }
        return groups.sortedWith(ORDERING)
    }

    private fun groupOf(
        hash: String?,
        cohort: DeviceCohort?,
        members: List<Session>,
    ): SessionCohortGroup {
        val ordered = members.sortedByDescending(Session::lastActiveAt)
        return SessionCohortGroup(
            cohortHash = hash,
            firstSeen = cohort?.firstSeen ?: ordered.minOfOrNull(Session::createdAt),
            lastSeen = cohort?.lastSeen ?: ordered.maxOfOrNull(Session::lastActiveAt),
            sessions = ordered,
        )
    }

    private val ORDERING =
        compareByDescending<SessionCohortGroup> { it.isCurrent }
            .thenByDescending { it.cohortHash != null }
            .thenByDescending { it.lastSeen ?: Instant.EPOCH }
}
