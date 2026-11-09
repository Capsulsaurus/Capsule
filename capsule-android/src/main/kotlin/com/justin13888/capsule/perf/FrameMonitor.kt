package com.justin13888.capsule.perf

import android.view.Window
import androidx.metrics.performance.JankStats
import java.util.logging.Logger

/**
 * Production jank telemetry for one activity window, through JankStats.
 *
 * The parity benchmark measures frames on a bench device; this is the same signal from the
 * field, summarized per [JankTally] window into one `capsule.perf` log line. Track only while
 * the activity is resumed: a paused window draws nothing worth judging.
 */
class FrameMonitor(
    window: Window,
    windowFrames: Int = JankTally.DEFAULT_WINDOW_FRAMES,
) {
    private val tally = JankTally(windowFrames, ::log)
    private val jankStats =
        JankStats.createAndTrack(window) { frame ->
            tally.record(frame.frameDurationUiNanos, frame.isJank)
        }

    /** Resume tracking (call from `onResume`). */
    fun resume() {
        jankStats.isTrackingEnabled = true
    }

    /** Stop tracking and report the partial window (call from `onPause`). */
    fun pause() {
        jankStats.isTrackingEnabled = false
        tally.flush()
    }

    private fun log(summary: JankSummary) {
        // Structured key=value so the line is queryable from logcat.
        logger.info(
            "frames.window frames=${summary.frames} janky=${summary.jankyFrames} " +
                "janky_pct=${"%.2f".format(java.util.Locale.ROOT, summary.jankyPercent)} " +
                "worst_ms=${summary.worstFrameNanos / NANOS_PER_MILLI}",
        )
    }

    private companion object {
        const val NANOS_PER_MILLI = 1_000_000L
        val logger: Logger = Logger.getLogger("capsule.perf")
    }
}
