package com.justin13888.capsule.core.ui.gallery

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * The catalog reaches Android with its ICU placeholders intact, so substitution happens
 * here — and a substitution bug shows up as a `{count}` in a screen reader announcement,
 * which nobody sighted will ever notice.
 */
class GalleryMessageTest {
    @Test
    fun `named placeholders are filled`() {
        assertEquals("Stack of 7", "Stack of {count}".withArguments("count" to 7))
        assertEquals(
            "12 March, 41 photos",
            "{label}, {count} photos".withArguments("label" to "12 March", "count" to 41),
        )
    }

    @Test
    fun `an unknown placeholder is left visible rather than dropped`() {
        assertEquals("Stack of {count}", "Stack of {count}".withArguments("other" to 1))
    }

    @Test
    fun `a substituted value containing braces is not expanded again`() {
        assertEquals("Video, {count}", "Video, {duration}".withArguments("duration" to "{count}", "count" to 9))
    }

    @Test
    fun `a message without placeholders is returned untouched`() {
        val message = "Upload failed"
        assertEquals(message, message.withArguments("count" to 1))
    }

    @Test
    fun `an unterminated placeholder is left alone`() {
        assertEquals("Stack of {count", "Stack of {count".withArguments("count" to 3))
    }
}
