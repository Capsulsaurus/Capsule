package com.justin13888.capsule.feature.security

import com.justin13888.capsule.core.model.Session
import java.time.format.DateTimeFormatter

/**
 * The dispute path for a device grouping the user does not recognise.
 *
 * There is deliberately no "this isn't my device" toggle. The grouping hash is client-asserted
 * and unverifiable, so a user cannot adjudicate it and a toggle would only record an opinion
 * nothing could act on. What a person *can* act on is the exact hash and the device and
 * session ids under it, so the dispute path is one tap that produces precisely that.
 *
 * Deliberately narrow: the hash, and one line per session. Nothing about the library, nothing
 * about the account beyond what the session ledger already shows the user on the same screen.
 */
object SupportBundle {
    /**
     * Renders the report for one cohort group.
     *
     * Timestamps are ISO-8601 in UTC rather than the reader's locale: this text is read by
     * whoever is helping, alongside server-side records that are also in UTC, and a date
     * formatted for the reporter's locale is one more thing for them to convert.
     */
    fun render(group: SessionCohortGroup): String {
        val lines = mutableListOf(HEADER + (group.cohortHash ?: NONE))
        group.firstSeen?.let { lines += FIRST_SEEN + ISO.format(it) }
        group.lastSeen?.let { lines += LAST_SEEN + ISO.format(it) }
        group.sessions.forEach { session -> lines += sessionLine(session) }
        return lines.joinToString(separator = "\n")
    }

    private fun sessionLine(session: Session): String =
        listOf(
            DEVICE + (session.deviceId?.value ?: NONE),
            SESSION + session.id.value,
            CREATED + ISO.format(session.createdAt),
            ACTIVE + ISO.format(session.lastActiveAt),
        ).joinToString(separator = " ")

    private val ISO: DateTimeFormatter = DateTimeFormatter.ISO_INSTANT

    private const val HEADER = "cohort_hash="
    private const val FIRST_SEEN = "first_seen="
    private const val LAST_SEEN = "last_seen="
    private const val DEVICE = "device_id="
    private const val SESSION = "session_id="
    private const val CREATED = "first_seen="
    private const val ACTIVE = "last_seen="
    private const val NONE = "none"
}
