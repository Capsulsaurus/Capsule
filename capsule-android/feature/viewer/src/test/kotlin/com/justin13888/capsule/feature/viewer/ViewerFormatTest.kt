package com.justin13888.capsule.feature.viewer

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import java.time.Duration
import java.util.Locale

/**
 * The renderings no catalog key can carry.
 *
 * Small, but worth pinning: a clock that padded to hours would put `0:01:23` under every clip
 * in a phone library, and a placeholder substitution that rescanned its own output would
 * expand a brace inside a user-typed tag. Both are the kind of defect that only shows up in
 * front of real data.
 */
class ViewerFormatTest {
    @Test
    fun `a clip under an hour reads as minutes and seconds`() {
        assertEquals("1:23", ViewerFormat.clock(Duration.ofSeconds(83)))
        assertEquals("0:05", ViewerFormat.clock(Duration.ofSeconds(5)))
    }

    @Test
    fun `a clip over an hour grows a third field`() {
        assertEquals("1:02:03", ViewerFormat.clock(Duration.ofSeconds(3723)))
    }

    @Test
    fun `a negative running time floors at zero rather than reading backwards`() {
        assertEquals("0:00", ViewerFormat.clock(Duration.ofSeconds(-5)))
    }

    @Test
    fun `a digest is abbreviated with an ellipsis and short values are left alone`() {
        val digest = "0123456789abcdef0123"

        assertEquals("0123456789ab…", ViewerFormat.shortHash(digest))
        assertEquals("abc", ViewerFormat.shortHash("abc"))
    }

    @Test
    fun `upload progress is a whole percentage clamped to the range`() {
        assertEquals(50, ViewerFormat.percent(sent = 5, total = 10))
        assertEquals(100, ViewerFormat.percent(sent = 20, total = 10))
        assertEquals(0, ViewerFormat.percent(sent = 5, total = 0))
    }

    @Test
    fun `a coordinate keeps five decimal places`() {
        assertEquals("51.50735", ViewerFormat.coordinate(51.5073509, Locale.ROOT))
    }

    @Test
    fun `placeholders are filled from the arguments given`() {
        val filled = "{position} of {total}".withArguments("position" to 3, "total" to 240)

        assertEquals("3 of 240", filled)
    }

    @Test
    fun `an unknown placeholder is left verbatim rather than dropped`() {
        val filled = "{a} and {b}".withArguments("a" to "one")

        assertEquals("one and {b}", filled)
    }

    @Test
    fun `a substituted value containing braces is not expanded again`() {
        val filled = "{tag}".withArguments("tag" to "{position}", "position" to "leaked")

        assertEquals("{position}", filled)
    }
}
