package com.justin13888.capsule.feature.viewer

import com.justin13888.capsule.feature.viewer.media.PlaybackController
import com.justin13888.capsule.feature.viewer.media.PlaybackState
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Duration

/**
 * Playback with the clock removed.
 *
 * There is no decoder behind the viewer, so a clip is a duration and a position — which turns
 * out to be enough to specify every behaviour a transport control owes, and to check all of
 * them in microseconds instead of minutes. The cases that matter are the edges: what happens
 * at the end, and what pressing play means once you are already there.
 */
class PlaybackTest {
    private val clip = PlaybackState(duration = Duration.ofSeconds(30))

    @Test
    fun `a paused clip ignores the clock`() {
        val advanced = clip.advanced(Duration.ofSeconds(5))

        assertEquals(Duration.ZERO, advanced.position)
    }

    @Test
    fun `a playing clip advances by the elapsed time`() {
        val advanced = clip.toggled().advanced(Duration.ofSeconds(5))

        assertEquals(Duration.ofSeconds(5), advanced.position)
        assertTrue(advanced.isPlaying)
    }

    @Test
    fun `reaching the end stops rather than looping`() {
        val finished = clip.toggled().advanced(Duration.ofMinutes(1))

        assertEquals(clip.duration, finished.position)
        assertFalse(finished.isPlaying) { "a clip that ran out should not keep the screen awake" }
        assertTrue(finished.isFinished)
    }

    @Test
    fun `pressing play on a finished clip rewinds it first`() {
        val finished = clip.toggled().advanced(Duration.ofMinutes(1))

        val replayed = finished.toggled()

        assertEquals(Duration.ZERO, replayed.position)
        assertTrue(replayed.isPlaying)
    }

    @Test
    fun `pressing pause while playing stops without moving the position`() {
        val playing = clip.toggled().advanced(Duration.ofSeconds(8))

        val paused = playing.toggled()

        assertEquals(Duration.ofSeconds(8), paused.position)
        assertFalse(paused.isPlaying)
    }

    @Test
    fun `seeking clamps to the clip and leaves the transport alone`() {
        val playing = clip.toggled()

        assertEquals(clip.duration, playing.seekTo(9f).position)
        assertEquals(Duration.ZERO, playing.seekTo(-3f).position)
        assertTrue(playing.seekTo(0.5f).isPlaying)
    }

    @Test
    fun `progress is zero for a clip with no running time`() {
        assertEquals(0f, PlaybackState(duration = Duration.ZERO).progress)
    }

    @Test
    fun `the controller applies ticks from whatever clock it is given`() {
        val controller = PlaybackController(clip)
        controller.toggle()

        repeat(4) { controller.advance(Duration.ofMillis(250)) }

        assertEquals(Duration.ofSeconds(1), controller.state.position)
    }
}
