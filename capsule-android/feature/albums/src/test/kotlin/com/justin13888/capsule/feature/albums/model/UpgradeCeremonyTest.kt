package com.justin13888.capsule.feature.albums.model

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Duration
import java.time.Instant

/**
 * The album upgrade ceremony, phase by phase.
 *
 * The ordering *is* the safety property: writes are quiesced before the drain, the drain
 * completes before the tombstone, and the tombstone is the single atomic cutover. Every test
 * below pins one link of that chain, plus the two ways it is allowed to end early — a
 * deadline the server's clock says has passed, and a member whose copy of the album
 * disagrees.
 */
class UpgradeCeremonyTest {
    private val now: Instant = Instant.parse("2026-06-01T00:00:00Z")
    private val intent = "intent-1"
    private val target = "2026-06-01"

    private fun advance(
        phase: UpgradePhase,
        inFlight: Int = 0,
        agree: Boolean = true,
        at: Instant = now,
    ) = UpgradeCeremony.advance(
        current = phase,
        inFlightUploads = inFlight,
        frozenStateHash = "frozen",
        membersAgree = agree,
        toVersion = target,
        now = at,
    )

    @Test
    fun `the happy path walks quiesce, drain, tombstone, fork, complete`() {
        val quiescing = requireNotNull(UpgradeCeremony.propose(UpgradePhase.Idle, intent, now))
        assertInstanceOf(UpgradePhase.Quiescing::class.java, quiescing)

        val draining = advance(quiescing)
        assertInstanceOf(UpgradePhase.Draining::class.java, draining)

        val tombstoning = advance(draining)
        assertInstanceOf(UpgradePhase.Tombstoning::class.java, tombstoning)

        val forking = advance(tombstoning)
        assertInstanceOf(UpgradePhase.Forking::class.java, forking)

        val complete = advance(forking)
        assertEquals(UpgradePhase.Complete(intent, target), complete)
    }

    @Test
    fun `the drain does not finish while an upload is still in flight`() {
        val draining = advance(requireNotNull(UpgradeCeremony.propose(UpgradePhase.Idle, intent, now)))
        val stillDraining = advance(draining, inFlight = 3)
        assertEquals(UpgradePhase.Draining(intent, (draining as UpgradePhase.Draining).deadline, 0, 3), stillDraining)
    }

    @Test
    fun `a member whose frozen state differs aborts the upgrade`() {
        val tombstoning = UpgradePhase.Tombstoning(intent, "frozen", queuedWrites = 0)
        val aborted = advance(tombstoning, agree = false)
        assertEquals(UpgradePhase.Aborted(intent, UpgradeAbortReason.FrozenStateMismatch), aborted)
    }

    @Test
    fun `an expired deadline aborts cleanly rather than holding the album frozen`() {
        val quiescing = requireNotNull(UpgradeCeremony.propose(UpgradePhase.Idle, intent, now))
        val past = now.plus(UpgradeCeremony.DEFAULT_DEADLINE).plus(Duration.ofSeconds(1))
        assertEquals(UpgradePhase.Aborted(intent, UpgradeAbortReason.DeadlineExpired), advance(quiescing, at = past))
    }

    @Test
    fun `only one upgrade may be in flight per album`() {
        val quiescing = requireNotNull(UpgradeCeremony.propose(UpgradePhase.Idle, intent, now))
        assertNull(UpgradeCeremony.propose(quiescing, "intent-2", now))
    }

    @Test
    fun `a second proposal is allowed once the first has ended`() {
        val aborted = UpgradePhase.Aborted(intent, UpgradeAbortReason.AbandonedByUser)
        assertInstanceOf(UpgradePhase.Quiescing::class.java, UpgradeCeremony.propose(aborted, "intent-2", now))
    }

    @Test
    fun `aborting before the cutover returns the album to normal`() {
        val quiescing = requireNotNull(UpgradeCeremony.propose(UpgradePhase.Idle, intent, now))
        assertEquals(UpgradePhase.Aborted(intent, UpgradeAbortReason.AbandonedByUser), UpgradeCeremony.abort(quiescing))
    }

    @Test
    fun `aborting after the fork is a no-op, because there is nothing left to abort`() {
        val complete = UpgradePhase.Complete(intent, target)
        assertEquals(complete, UpgradeCeremony.abort(complete))
    }

    @Test
    fun `writes made during quiescence are counted so they can be replayed`() {
        val quiescing = requireNotNull(UpgradeCeremony.propose(UpgradePhase.Idle, intent, now))
        val queued = UpgradeCeremony.queueWrite(UpgradeCeremony.queueWrite(quiescing))
        assertEquals(2, (queued as UpgradePhase.Quiescing).queuedWrites)
        assertTrue(queued.isRunning)
    }
}
