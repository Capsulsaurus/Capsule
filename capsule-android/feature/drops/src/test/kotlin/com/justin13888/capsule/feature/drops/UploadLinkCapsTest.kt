package com.justin13888.capsule.feature.drops

import com.justin13888.capsule.core.data.DropError
import com.justin13888.capsule.core.data.LocalError
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Duration
import java.time.Instant

/**
 * The caps a form describes and the usage a detail screen reads back.
 *
 * Every one of these is arithmetic a user cannot check, and every one bounds what a leaked
 * link is able to spend — an empty field that quietly became a zero ceiling, or a megabyte
 * that turned into a byte, would each be a link that refuses everything or accepts anything.
 */
class UploadLinkCapsTest {
    private val now: Instant = Instant.parse("2026-06-15T19:00:00Z")

    @Test
    fun `an empty cap means unbounded rather than zero`() {
        val entry = CapEntry("")
        assertTrue(entry.isUnbounded)
        assertFalse(entry.isInvalid)
        assertNull(entry.value)
    }

    @Test
    fun `a cap that is neither empty nor a positive number is an error the form shows`() {
        for (raw in listOf("abc", "-1", "0", "1.5", "1 000")) {
            assertTrue(CapEntry(raw).isInvalid, "'$raw' should not be accepted as a ceiling")
        }
        assertFalse(CapEntry(" 12 ").isInvalid, "surrounding space is not a typo worth refusing")
        assertEquals(12L, CapEntry(" 12 ").value)
    }

    @Test
    fun `the form converts its megabyte fields into the bytes the contract takes`() {
        val state =
            UploadLinkCreateUiState(
                expiry = LinkExpiry.OneWeek,
                maxTotal = CapEntry("500"),
                maxFiles = CapEntry("20"),
                maxFileSize = CapEntry("25"),
                singleUse = true,
            )
        val caps = state.caps(now)
        assertEquals(500L * BYTES_PER_MEGABYTE, caps.maxTotalBytes)
        assertEquals(20, caps.maxFileCount)
        assertEquals(25L * BYTES_PER_MEGABYTE, caps.maxFileSize)
        assertTrue(caps.singleUse)
        assertEquals(now.plus(Duration.ofDays(7)), caps.expiresAt)
    }

    @Test
    fun `an unbounded field reaches the contract as no ceiling at all`() {
        val caps = UploadLinkCreateUiState(expiry = LinkExpiry.Never).caps(now)
        assertNull(caps.maxTotalBytes)
        assertNull(caps.maxFileCount)
        assertNull(caps.maxFileSize)
        assertNull(caps.expiresAt)
        assertFalse(caps.singleUse)
    }

    @Test
    fun `a half-typed cap blocks submission rather than being rounded away`() {
        val state = UploadLinkCreateUiState(maxFiles = CapEntry("twelve"))
        assertTrue(state.hasInvalidCap)
        assertFalse(state.canSubmit)
    }

    @Test
    fun `usage against an absent ceiling is not the same as an empty one`() {
        val unbounded = CapUsage(used = 400, limit = null)
        assertEquals(0f, unbounded.fraction)
        assertFalse(unbounded.isExhausted, "a cap that was never set cannot be full")

        val half = CapUsage(used = 50, limit = 100)
        assertEquals(0.5f, half.fraction)
        assertFalse(half.isExhausted)

        val full = CapUsage(used = 120, limit = 100)
        assertEquals(1f, full.fraction, "an overshoot still draws as full, never past it")
        assertTrue(full.isExhausted)
    }

    @Test
    fun `offline is classified apart from a server refusal`() {
        assertEquals(DropsFailureKind.Offline, DropsFailure.of(LocalError.Offline).kind)

        val reported = DropsFailure.of(DropError.CapExceeded)
        assertEquals(DropsFailureKind.Reported, reported.kind)
        assertEquals("error.drop.cap_exceeded", reported.code?.value)

        val unknown = DropsFailure.of(IllegalStateException("boom"))
        assertEquals(DropsFailureKind.Unknown, unknown.kind)
        assertNull(unknown.code)
    }
}
