package com.justin13888.capsule.perf

/** Frames observed over one reporting window. */
data class JankSummary(
    val frames: Int,
    val jankyFrames: Int,
    val worstFrameNanos: Long,
) {
    /** Share of the window's frames that JankStats judged janky, 0 to 100. */
    val jankyPercent: Double
        get() = if (frames == 0) 0.0 else 100.0 * jankyFrames / frames
}

/**
 * Counts frames into fixed-size windows and hands each full window to [report].
 *
 * JankStats calls back once per frame, so logging each janky frame would flood the log during
 * exactly the scroll it is meant to explain; a window summary is one structured line per
 * [windowFrames] frames instead. A window that holds no jank is still reported, because "the
 * last ten seconds were clean" is the evidence that a regression has not shipped.
 *
 * Plain Kotlin over plain numbers, so it is unit-tested without a device; [FrameMonitor] feeds
 * it from JankStats.
 */
class JankTally(
    private val windowFrames: Int = DEFAULT_WINDOW_FRAMES,
    private val report: (JankSummary) -> Unit,
) {
    private var frames = 0
    private var jankyFrames = 0
    private var worstFrameNanos = 0L

    init {
        require(windowFrames > 0) { "a reporting window holds at least one frame" }
    }

    /** Record one frame; reports and resets once the window is full. */
    fun record(
        durationNanos: Long,
        isJank: Boolean,
    ) {
        frames += 1
        if (isJank) jankyFrames += 1
        if (durationNanos > worstFrameNanos) worstFrameNanos = durationNanos
        if (frames == windowFrames) flush()
    }

    /** Report a partly filled window (the screen is going away) and reset; no-op when empty. */
    fun flush() {
        if (frames == 0) return
        report(JankSummary(frames, jankyFrames, worstFrameNanos))
        frames = 0
        jankyFrames = 0
        worstFrameNanos = 0L
    }

    companion object {
        /** Ten seconds at 60 Hz. */
        const val DEFAULT_WINDOW_FRAMES = 600
    }
}
