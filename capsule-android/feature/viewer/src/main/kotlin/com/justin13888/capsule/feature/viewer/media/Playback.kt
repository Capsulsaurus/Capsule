package com.justin13888.capsule.feature.viewer.media

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import kotlinx.coroutines.delay
import java.time.Duration

/**
 * The source of elapsed time playback advances against.
 *
 * A seam rather than a `delay` buried in the composable, because this app has no decoder: the
 * mocked library synthesizes its imagery and there is no media pipeline to ask what second a
 * clip is on. Playback is therefore a clock and a position, and making the clock injectable
 * is what lets a test advance a two-minute video in three assertions instead of two minutes.
 */
fun interface PlaybackClock {
    /** Suspends until some time has passed, and reports how much. */
    suspend fun tick(): Duration
}

/**
 * The wall clock, ticking at roughly the rate a progress bar needs to look continuous.
 *
 * Deliberately coarse. A frame-accurate tick would recompose the control strip sixty times a
 * second to move a bar by a pixel, which is a lot of work to produce an effect nobody can see.
 */
object RealtimePlaybackClock : PlaybackClock {
    /** How long each tick waits. */
    val INTERVAL: Duration = Duration.ofMillis(200)

    override suspend fun tick(): Duration {
        delay(INTERVAL.toMillis())
        return INTERVAL
    }
}

/**
 * Where a clip is, and whether it is running.
 *
 * Every transition is a pure function returning a new value, so the whole of playback —
 * reaching the end, seeking past it, toggling while finished — is assertable without a
 * composition, a decoder, or a passing second.
 */
@Immutable
data class PlaybackState(
    /** The clip's running time. */
    val duration: Duration,
    /** How far in playback has reached. Never negative, never past [duration]. */
    val position: Duration = Duration.ZERO,
    /** Whether the clock is advancing the position. */
    val isPlaying: Boolean = false,
) {
    /** How far through the clip is, as `0..1`. Zero for a clip with no running time. */
    val progress: Float
        get() = if (duration.isZero || duration.isNegative) 0f else position.toMillis().toFloat() / duration.toMillis()

    /** Whether the position has reached the end. */
    val isFinished: Boolean get() = position >= duration

    /**
     * Advances by [elapsed], stopping at the end.
     *
     * Reaching the end clears [isPlaying] rather than looping. A viewer that silently looped
     * would make a one-second clip impossible to tell from a still, and would keep the screen
     * awake for as long as the user left the app open.
     */
    fun advanced(elapsed: Duration): PlaybackState {
        if (!isPlaying) return this
        val next = position.plus(elapsed)
        return if (next >= duration) copy(position = duration, isPlaying = false) else copy(position = next)
    }

    /**
     * Starts, pauses, or restarts.
     *
     * Pressing play on a finished clip rewinds it first — otherwise the button would appear
     * to do nothing, which is the single most common way a media control feels broken.
     */
    fun toggled(): PlaybackState =
        when {
            isPlaying -> copy(isPlaying = false)
            isFinished -> copy(position = Duration.ZERO, isPlaying = true)
            else -> copy(isPlaying = true)
        }

    /** Seeks to a fraction of the clip, clamped to it. Seeking does not change [isPlaying]. */
    fun seekTo(fraction: Float): PlaybackState {
        val clamped = fraction.coerceIn(0f, 1f)
        return copy(position = Duration.ofMillis((duration.toMillis() * clamped).toLong()))
    }
}

/**
 * A [PlaybackState] that advances itself while it is playing.
 *
 * The loop lives here rather than in the state so the state stays a value: a data class that
 * owned a coroutine could not be compared, and every recomposition would look like a change.
 *
 * @param duration the clip's running time.
 * @param clock the source of elapsed time. Injectable so a test never has to wait.
 */
@Composable
fun rememberPlaybackState(
    duration: Duration,
    clock: PlaybackClock = RealtimePlaybackClock,
): PlaybackController {
    val controller = remember(duration) { PlaybackController(PlaybackState(duration)) }
    LaunchedEffect(controller, controller.state.isPlaying, clock) {
        while (controller.state.isPlaying) {
            controller.advance(clock.tick())
        }
    }
    return controller
}

/**
 * The mutable handle a control strip drives, wrapping an immutable [PlaybackState].
 *
 * `@Stable` rather than `@Immutable`: the value inside changes, but the identity does not, so
 * Compose can skip everything that does not read [state].
 */
@Stable
class PlaybackController(
    initial: PlaybackState,
) {
    /** The current position and running state. */
    var state: PlaybackState by mutableStateOf(initial)
        private set

    /** Starts, pauses, or restarts from the beginning when the clip has finished. */
    fun toggle() {
        state = state.toggled()
    }

    /** Moves to a fraction of the clip. */
    fun seekTo(fraction: Float) {
        state = state.seekTo(fraction)
    }

    /** Applies one tick of the clock. */
    fun advance(elapsed: Duration) {
        state = state.advanced(elapsed)
    }
}
