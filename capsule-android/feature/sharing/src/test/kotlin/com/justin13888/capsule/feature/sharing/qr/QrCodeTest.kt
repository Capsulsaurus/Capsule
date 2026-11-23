package com.justin13888.capsule.feature.sharing.qr

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The QR encoder, held to the standard rather than to itself.
 *
 * What matters here is not that a picture appears: it is that the *exact* string handed in —
 * fragment secret and all — is the string a scanner reads back out. Every other assertion in
 * this file exists to localise a failure of that one.
 */
class QrCodeTest {
    private val shareUrl =
        "https://capsule.example/s/8f14e45fceea167a5a36dedd4bea2543#c9f0f895fb98ab9159f51fd0297e236d"

    @Test
    fun `a share URL survives the round trip with its fragment intact`() {
        val code = requireNotNull(QrCode.encode(shareUrl))
        val decoded = QrReference.decode(code)
        assertEquals(shareUrl, decoded)
        assertTrue(decoded.contains('#'), "the fragment is what decrypts the share")
        assertEquals(shareUrl.substringAfter('#'), decoded.substringAfter('#'))
    }

    @Test
    fun `every text length up to the version ten ceiling round-trips`() {
        // Walks the version boundaries, which is where a capacity or block-table mistake
        // hides: one byte either side of a version change encodes into a different symbol.
        for (length in listOf(1, 14, 15, 26, 27, 84, 85, 106, 107, 180, 181, 213)) {
            val text = buildString { repeat(length) { append('a' + it % 26) } }
            val code = requireNotNull(QrCode.encode(text)) { "length $length should encode" }
            assertEquals(text, QrReference.decode(code), "length $length round trip")
        }
    }

    @Test
    fun `text beyond version ten is refused rather than truncated`() {
        assertNull(QrCode.encode("x".repeat(214)))
    }

    @Test
    fun `the smallest version that fits is the one chosen`() {
        assertEquals(1, requireNotNull(QrCode.encode("x".repeat(14))).version)
        assertEquals(2, requireNotNull(QrCode.encode("x".repeat(15))).version)
        assertEquals(6, requireNotNull(QrCode.encode("x".repeat(106))).version)
        assertEquals(7, requireNotNull(QrCode.encode("x".repeat(107))).version)
    }

    @Test
    fun `the symbol declares level M and the mask it was actually drawn with`() {
        val code = requireNotNull(QrCode.encode(shareUrl))
        assertEquals(0, QrReference.readLevel(code), "level M is the two-bit value 0")
        val mask = QrReference.readMask(code)
        assertTrue(mask in 0 until 8, "declared mask $mask is outside the defined range")
    }

    @Test
    fun `the finder patterns and the dark module are where a scanner looks for them`() {
        val code = requireNotNull(QrCode.encode(shareUrl))
        val last = code.size - 1
        for (corner in listOf(0 to 0, last - 6 to 0, 0 to last - 6)) {
            val (x, y) = corner
            assertTrue(code.isDark(x, y), "finder outer ring at $corner")
            assertTrue(code.isDark(x + 3, y + 3), "finder centre at $corner")
            // The light ring is what separates the eye from its border.
            assertTrue(!code.isDark(x + 1, y + 3), "finder inner light ring at $corner")
        }
        assertTrue(code.isDark(8, code.size - 8), "the always-dark module")
    }

    @Test
    fun `the timing patterns alternate the whole way across`() {
        val code = requireNotNull(QrCode.encode(shareUrl))
        for (index in 8 until code.size - 8) {
            assertEquals(index % 2 == 0, code.isDark(index, 6), "horizontal timing at $index")
            assertEquals(index % 2 == 0, code.isDark(6, index), "vertical timing at $index")
        }
    }
}
