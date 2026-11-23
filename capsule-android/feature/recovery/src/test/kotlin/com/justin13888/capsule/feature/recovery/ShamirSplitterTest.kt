package com.justin13888.capsule.feature.recovery

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import java.security.SecureRandom

/**
 * The split itself.
 *
 * Worth testing directly rather than through the screen, because a split that produces shares
 * which do not recombine is a failure nobody notices until the day it matters — the user has
 * handed out three envelopes and finds out years later that two of them rebuild nothing. Every
 * subset of the threshold size is checked, not just the first one, since the classic
 * implementation bug (sign-extending a signed byte) breaks only some subsets.
 */
class ShamirSplitterTest {
    private val random = SecureRandom.getInstance("SHA1PRNG").apply { setSeed(SEED) }

    @Test
    fun `any two of three shares rebuild the secret`() {
        val shares = ShamirSplitter.split(SECRET, threshold = 2, total = 3, random = random)

        assertEquals(3, shares.size)
        shares.indices.forEach { first ->
            (first + 1 until shares.size).forEach { second ->
                assertArrayEquals(
                    SECRET,
                    ShamirSplitter.combine(listOf(shares[first], shares[second])),
                    "shares ${shares[first].x} and ${shares[second].x} should rebuild it",
                )
            }
        }
    }

    @Test
    fun `a custom three of five scheme works the same way`() {
        val shares = ShamirSplitter.split(SECRET, threshold = 3, total = 5, random = random)

        assertArrayEquals(SECRET, ShamirSplitter.combine(shares.take(3)))
        assertArrayEquals(SECRET, ShamirSplitter.combine(shares.takeLast(3)))
        assertArrayEquals(SECRET, ShamirSplitter.combine(listOf(shares[0], shares[2], shares[4])))
    }

    @Test
    fun `every share is used, so all of them together still rebuild it`() {
        val shares = ShamirSplitter.split(SECRET, threshold = 3, total = 5, random = random)

        assertArrayEquals(SECRET, ShamirSplitter.combine(shares))
    }

    @Test
    fun `one share short of the threshold rebuilds something else`() {
        val shares = ShamirSplitter.split(SECRET, threshold = 3, total = 5, random = random)

        assertFalse(
            ShamirSplitter.combine(shares.take(2)).contentEquals(SECRET),
            "two of three must not be enough, or the scheme is not the scheme",
        )
    }

    @Test
    fun `a share alone reveals nothing that looks like the secret`() {
        val shares = ShamirSplitter.split(SECRET, threshold = 2, total = 3, random = random)

        shares.forEach { share ->
            assertFalse(share.payload.contentEquals(SECRET), "share ${share.x} is the secret in the clear")
        }
    }

    @Test
    fun `shares are the same length as the secret, so nothing is padded or truncated`() {
        val shares = ShamirSplitter.split(SECRET, threshold = 2, total = 4, random = random)

        assertTrue(shares.all { it.payload.size == SECRET.size })
        assertEquals(listOf(1, 2, 3, 4), shares.map { it.x })
    }

    @Test
    fun `a byte of zero survives the round trip`() {
        val withZero = byteArrayOf(0, 0, 127, -128, -1, 0)

        val shares = ShamirSplitter.split(withZero, threshold = 2, total = 3, random = random)

        assertArrayEquals(withZero, ShamirSplitter.combine(shares.take(2)))
    }

    @Test
    fun `a threshold of one is refused because it would not be a secret`() {
        assertThrows<IllegalArgumentException> {
            ShamirSplitter.split(SECRET, threshold = 1, total = 3, random = random)
        }
    }

    @Test
    fun `needing more shares than exist is refused`() {
        assertThrows<IllegalArgumentException> {
            ShamirSplitter.split(SECRET, threshold = 4, total = 3, random = random)
        }
    }

    @Test
    fun `combining nothing is refused rather than answering with an empty secret`() {
        assertThrows<IllegalArgumentException> { ShamirSplitter.combine(emptyList()) }
    }

    @Test
    fun `the same share supplied twice is refused rather than dividing by zero`() {
        val shares = ShamirSplitter.split(SECRET, threshold = 2, total = 3, random = random)

        assertThrows<IllegalArgumentException> {
            ShamirSplitter.combine(listOf(shares[0], shares[0]))
        }
    }

    @Test
    fun `shares of different secrets are refused rather than silently mixed`() {
        val first = ShamirSplitter.split(SECRET, threshold = 2, total = 3, random = random)
        val second = ShamirSplitter.split(byteArrayOf(1, 2, 3), threshold = 2, total = 3, random = random)

        assertThrows<IllegalArgumentException> {
            ShamirSplitter.combine(listOf(first[0], second[1]))
        }
    }

    private companion object {
        val SECRET = "amber anchor apple arrow basin beacon".toByteArray()
        val SEED = byteArrayOf(4, 8, 15, 16, 23, 42)
    }
}
