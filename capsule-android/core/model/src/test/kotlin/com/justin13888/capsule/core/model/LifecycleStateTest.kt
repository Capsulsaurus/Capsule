package com.justin13888.capsule.core.model

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Nested
import org.junit.jupiter.api.Test
import java.time.Duration
import java.time.Instant

/**
 * The states that move with the clock: trash retention, quota escalation, session expiry,
 * and the recovery-verification cadence.
 *
 * All four are derived from a supplied instant rather than from `Instant.now()`, which is
 * what makes them testable at all — and the reason they were written that way.
 */
class LifecycleStateTest {
    @Nested
    inner class TrashRetention {
        @Test
        fun `the countdown shrinks as the deadline approaches`() {
            val trashed = DeletionState.Trashed(deletedAt = T0, retentionUntil = T0.plus(THIRTY_DAYS))
            assertEquals(THIRTY_DAYS, trashed.retentionRemaining(T0))
            assertEquals(Duration.ofDays(15), trashed.retentionRemaining(T0.plus(Duration.ofDays(15))))
            assertEquals(Duration.ofDays(1), trashed.retentionRemaining(T0.plus(Duration.ofDays(29))))
        }

        @Test
        fun `the countdown floors at zero rather than going negative`() {
            // A negative "time remaining" would render as a countdown running backwards.
            val trashed = DeletionState.Trashed(T0, T0.plus(THIRTY_DAYS))
            assertEquals(Duration.ZERO, trashed.retentionRemaining(T0.plus(Duration.ofDays(31))))
        }

        @Test
        fun `a purge is refused until the signed deadline passes`() {
            // The floor is the signed manifest's, so nothing downstream can bring it forward.
            val trashed = DeletionState.Trashed(T0, T0.plus(THIRTY_DAYS))
            assertFalse(trashed.isPurgeable(T0))
            assertFalse(trashed.isPurgeable(T0.plus(Duration.ofDays(15))))
            assertFalse(trashed.isPurgeable(T0.plus(THIRTY_DAYS).minusMillis(1)))
            assertTrue(trashed.isPurgeable(T0.plus(THIRTY_DAYS)))
            assertTrue(trashed.isPurgeable(T0.plus(Duration.ofDays(31))))
        }

        @Test
        fun `the default retention window is thirty days`() {
            assertEquals(THIRTY_DAYS, DeletionState.DEFAULT_RETENTION)
        }

        @Test
        fun `an extended window is honoured over the default`() {
            val extended = DeletionState.Trashed(T0, T0.plus(Duration.ofDays(90)))
            assertFalse(extended.isPurgeable(T0.plus(Duration.ofDays(31))))
            assertTrue(extended.isPurgeable(T0.plus(Duration.ofDays(90))))
        }
    }

    @Nested
    inner class QuotaEscalation {
        @Test
        fun `usage below the soft limit is ok`() {
            assertEquals(QuotaState.Ok, status(used = 10).stateAt(T0))
        }

        @Test
        fun `crossing the soft limit warns without refusing uploads`() {
            val state = status(used = SOFT).stateAt(T0)
            assertEquals(QuotaState.SoftWarning, state)
            assertTrue(state.allowsUpload)
            assertTrue(state.allowsMetadataGrowth)
        }

        @Test
        fun `crossing the hard limit refuses uploads but keeps every other write`() {
            val state = status(used = HARD, crossedAt = T0).stateAt(T0)
            assertEquals(QuotaState.HardExceeded, state)
            assertFalse(state.allowsUpload)
            assertTrue(state.allowsMetadataGrowth, "metadata edits must keep working at the hard limit")
        }

        @Test
        fun `staying past the hard limit beyond the grace window pauses metadata growth`() {
            val quota = status(used = HARD, crossedAt = T0)
            assertEquals(QuotaState.HardExceeded, quota.stateAt(T0.plus(Duration.ofDays(13))))
            assertEquals(QuotaState.GraceExpired, quota.stateAt(T0.plus(QuotaStatus.DEFAULT_GRACE_WINDOW)))
        }

        @Test
        fun `the grace-expired state still admits the writes a delete produces`() {
            // A user must be able to delete their way back under quota; a state that blocked
            // deletion would be a trap with no exit.
            val state = status(used = HARD, crossedAt = T0).stateAt(T0.plus(Duration.ofDays(20)))
            assertEquals(QuotaState.GraceExpired, state)
            assertFalse(state.allowsMetadataGrowth)
            assertTrue(state.allowsRecoveryWrites)
        }

        @Test
        fun `suspension outranks every usage-derived state`() {
            val suspended = status(used = 0, isSuspended = true)
            assertEquals(QuotaState.Suspended, suspended.stateAt(T0))
            assertFalse(suspended.stateAt(T0).allowsUpload)
        }

        @Test
        fun `freeing space lifts the state again`() {
            val quota = status(used = HARD, crossedAt = T0)
            assertEquals(QuotaState.GraceExpired, quota.stateAt(T0.plus(Duration.ofDays(20))))
            val freed = quota.copy(used = 10, hardLimitCrossedAt = null)
            assertEquals(QuotaState.Ok, freed.stateAt(T0.plus(Duration.ofDays(20))))
        }

        @Test
        fun `an unlimited deployment never hard-exceeds`() {
            // A self-hosted server with no quota must not be mistaken for one that is full.
            val unlimited = QuotaStatus(used = Long.MAX_VALUE / 2, softLimit = SOFT, hardLimit = null)
            assertEquals(QuotaState.SoftWarning, unlimited.stateAt(T0))
            assertTrue(unlimited.admitsUpload(additionalBytes = 1_000_000, now = T0))
            assertNull(unlimited.remaining)
        }

        @Test
        fun `a session is admitted only if its declared size fits`() {
            val quota = status(used = HARD - 100)
            assertTrue(quota.admitsUpload(additionalBytes = 100, now = T0))
            assertFalse(quota.admitsUpload(additionalBytes = 101, now = T0))
        }

        @Test
        fun `remaining never reports a negative headroom`() {
            assertEquals(0L, status(used = HARD + 500, crossedAt = T0).remaining)
        }

        private fun status(
            used: Long,
            crossedAt: Instant? = null,
            isSuspended: Boolean = false,
        ) = QuotaStatus(
            used = used,
            softLimit = SOFT,
            hardLimit = HARD,
            hardLimitCrossedAt = crossedAt,
            isSuspended = isSuspended,
        )
    }

    @Nested
    inner class SessionExpiry {
        @Test
        fun `an active session is inside both bounds`() {
            assertTrue(session().isActiveAt(T0.plus(Duration.ofDays(10))))
        }

        @Test
        fun `inactivity ends a session even though the hard bound is far away`() {
            assertFalse(session().isActiveAt(T0.plus(Duration.ofDays(181))))
        }

        @Test
        fun `the hard bound ends a session that has been used continuously`() {
            // The sliding bound has been refreshed all the way to the hard one; the session
            // still ends, which is exactly what the second bound is for.
            val busy = session().copy(inactivityExpiresAt = T0.plus(Duration.ofDays(400)))
            assertFalse(busy.isActiveAt(T0.plus(Duration.ofDays(366))))
        }

        @Test
        fun `revocation ends a session regardless of either bound`() {
            assertFalse(session().copy(revokedAt = T0).isActiveAt(T0.plus(Duration.ofDays(1))))
        }

        @Test
        fun `the effective expiry is whichever bound arrives first`() {
            assertEquals(T0.plus(Duration.ofDays(180)), session().effectiveExpiry)
            val nearlyHard = session().copy(inactivityExpiresAt = T0.plus(Duration.ofDays(400)))
            assertEquals(T0.plus(Duration.ofDays(365)), nearlyHard.effectiveExpiry)
        }

        @Test
        fun `a local auth grant lapses after its window`() {
            val grant = LocalAuthGrant(GatedView.Hidden, grantedAt = T0)
            assertTrue(grant.isFreshAt(T0.plus(Duration.ofMinutes(4))))
            assertFalse(grant.isFreshAt(T0.plus(Duration.ofMinutes(5))))
            assertEquals(Duration.ofMinutes(5), LocalAuthGrant.DEFAULT_WINDOW)
        }

        @Test
        fun `grants are per view, so one does not open the other`() {
            val hidden = LocalAuthGrant(GatedView.Hidden, grantedAt = T0)
            assertEquals(GatedView.Hidden, hidden.view)
            assertTrue(GatedView.entries.containsAll(listOf(GatedView.Hidden, GatedView.RecentlyDeleted)))
        }

        private fun session() =
            Session(
                id = SessionId("s1"),
                userId = UserId("u1"),
                createdAt = T0,
                lastActiveAt = T0,
                inactivityExpiresAt = T0.plus(Session.DEFAULT_INACTIVITY_WINDOW),
                hardExpiresAt = T0.plus(Session.DEFAULT_HARD_LIFETIME),
            )
    }

    @Nested
    inner class RecoveryPrompting {
        @Test
        fun `the cadence backs off and then holds at its ceiling`() {
            assertEquals(RecoveryCadence.FIRST_INTERVAL, RecoveryCadence.intervalAfter(0))
            assertEquals(RecoveryCadence.SECOND_INTERVAL, RecoveryCadence.intervalAfter(1))
            assertEquals(RecoveryCadence.MAX_INTERVAL, RecoveryCadence.intervalAfter(2))
            assertEquals(RecoveryCadence.MAX_INTERVAL, RecoveryCadence.intervalAfter(50))
        }

        @Test
        fun `an untracked history takes the conservative short interval`() {
            assertEquals(RecoveryCadence.FIRST_INTERVAL, RecoveryCadence.intervalAfter(-1))
        }

        @Test
        fun `the intervals are the ones the design pins`() {
            assertEquals(Duration.ofDays(7), RecoveryCadence.FIRST_INTERVAL)
            assertEquals(Duration.ofDays(90), RecoveryCadence.SECOND_INTERVAL)
            assertEquals(Duration.ofDays(180), RecoveryCadence.MAX_INTERVAL)
            assertEquals(3, RecoveryCadence.MAX_CONSECUTIVE_SNOOZES)
            assertEquals(3, RecoveryCadence.FAILURES_BEFORE_REWRAP)
        }
    }

    private companion object {
        val T0: Instant = Instant.parse("2026-01-01T00:00:00Z")
        val THIRTY_DAYS: Duration = Duration.ofDays(30)
        const val SOFT = 800L
        const val HARD = 1000L
    }
}
