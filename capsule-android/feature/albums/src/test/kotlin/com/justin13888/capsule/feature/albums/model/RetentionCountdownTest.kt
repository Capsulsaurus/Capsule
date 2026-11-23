package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.DeletionState
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import java.time.Duration
import java.time.Instant

/**
 * The countdown in front of a signed retention window.
 *
 * The boundary that matters is expiry: past `retention_until` the purge worker may proceed,
 * so a countdown that still said "1 day left" would be telling the user a photograph is
 * recoverable when it is not. Rounding is downward everywhere for the same reason.
 */
class RetentionCountdownTest {
    private val deletedAt: Instant = Instant.parse("2026-06-01T00:00:00Z")

    private fun trashed(retention: Duration) = DeletionState.Trashed(deletedAt, deletedAt.plus(retention))

    @Test
    fun `more than a day remaining reports whole days, rounded down`() {
        val countdown = RetentionCountdown.of(trashed(Duration.ofHours(59)), deletedAt)
        assertEquals(RetentionCountdown.Days(2), countdown)
    }

    @Test
    fun `less than a day remaining reports hours`() {
        val countdown = RetentionCountdown.of(trashed(Duration.ofMinutes(90)), deletedAt)
        assertEquals(RetentionCountdown.Hours(1), countdown)
    }

    @Test
    fun `the instant the window closes is already expired`() {
        val state = trashed(Duration.ofDays(30))
        assertEquals(RetentionCountdown.Expired, RetentionCountdown.of(state, state.retentionUntil))
    }

    @Test
    fun `past the window stays expired rather than going negative`() {
        val state = trashed(Duration.ofDays(30))
        val later = state.retentionUntil.plus(Duration.ofDays(400))
        assertEquals(RetentionCountdown.Expired, RetentionCountdown.of(state, later))
    }
}
