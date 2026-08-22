package com.justin13888.capsule.security

import com.justin13888.capsule.core.model.GatedView
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Clock
import java.time.Duration
import java.time.Instant
import java.time.ZoneOffset

/**
 * The three properties SR1 actually promises, checked without waiting five minutes.
 *
 * A grace window is only meaningful if it expires, and per-view grants are only meaningful if
 * they do not leak into each other. Both are invisible in manual testing — a tester who
 * unlocks Trash and then opens Hidden a second later cannot tell a correct implementation
 * from one sharing a single flag — which is exactly why they are pinned here.
 */
class LocalAuthGateTest {
    private var now: Instant = Instant.parse("2026-08-22T09:00:00Z")
    private val gate = LocalAuthGate(MovingClock())

    @Test
    fun `a view starts locked`() {
        assertFalse(gate.isOpen(GatedView.RecentlyDeleted))
        assertFalse(gate.isOpen(GatedView.Hidden))
    }

    @Test
    fun `a grant opens its view`() {
        gate.grant(GatedView.RecentlyDeleted)

        assertTrue(gate.isOpen(GatedView.RecentlyDeleted))
    }

    @Test
    fun `a grant does not open the other view`() {
        gate.grant(GatedView.RecentlyDeleted)

        assertFalse(gate.isOpen(GatedView.Hidden), "unlocking the trash also unlocked Hidden")
    }

    @Test
    fun `a grant is still fresh just inside the window`() {
        gate.grant(GatedView.Hidden)
        now = now.plus(Duration.ofMinutes(4)).plusSeconds(59)

        assertTrue(gate.isOpen(GatedView.Hidden))
    }

    @Test
    fun `a grant has expired at the window`() {
        gate.grant(GatedView.Hidden)
        now = now.plus(Duration.ofMinutes(5))

        assertFalse(gate.isOpen(GatedView.Hidden))
    }

    @Test
    fun `re-authenticating restarts the window`() {
        gate.grant(GatedView.Hidden)
        now = now.plus(Duration.ofMinutes(6))
        assertFalse(gate.isOpen(GatedView.Hidden))

        gate.grant(GatedView.Hidden)

        assertTrue(gate.isOpen(GatedView.Hidden))
    }

    @Test
    fun `backgrounding drops every grant`() {
        gate.grant(GatedView.RecentlyDeleted)
        gate.grant(GatedView.Hidden)

        gate.revokeAll()

        assertFalse(gate.isOpen(GatedView.RecentlyDeleted))
        assertFalse(gate.isOpen(GatedView.Hidden))
    }

    /** A clock that reads whatever the test last set, so the window can be crossed instantly. */
    private inner class MovingClock : Clock() {
        override fun getZone(): ZoneOffset = ZoneOffset.UTC

        override fun withZone(zone: java.time.ZoneId?): Clock = this

        override fun instant(): Instant = now
    }
}
