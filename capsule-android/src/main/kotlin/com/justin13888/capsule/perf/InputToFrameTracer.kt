package com.justin13888.capsule.perf

import android.view.Choreographer
import androidx.tracing.Trace

/** An async trace section: begun and ended on different frames, matched by cookie. */
interface AsyncSection {
    fun begin(
        name: String,
        cookie: Int,
    )

    fun end(
        name: String,
        cookie: Int,
    )
}

/** [AsyncSection] over `androidx.tracing`, which is a no-op unless a trace is being recorded. */
object TracingSection : AsyncSection {
    override fun begin(
        name: String,
        cookie: Int,
    ) = Trace.beginAsyncSection(name, cookie)

    override fun end(
        name: String,
        cookie: Int,
    ) = Trace.endAsyncSection(name, cookie)
}

/**
 * Marks each tap's input-to-frame latency as the [BenchmarkAnchors.INPUT_TO_FRAME_SECTION]
 * trace section, which the parity benchmark reads with a `TraceSectionMetric`.
 *
 * The section opens when the tap is dispatched and closes at the start of the frame *after*
 * the one that answers it. A frame callback posted while the input is handled runs in that
 * same frame's animation phase, before it draws; the second hop runs once it has been drawn on
 * the UI thread. That is the "visible response in the next frame" the responsiveness contract
 * bounds, measured from the app's side.
 *
 * @param postFrame schedules a callback for the next frame (Choreographer in production).
 */
class InputToFrameTracer(
    private val section: AsyncSection = TracingSection,
    private val postFrame: (() -> Unit) -> Unit = { callback ->
        Choreographer.getInstance().postFrameCallback { callback() }
    },
) {
    private var nextCookie = 0

    /** A tap was dispatched; call on the main thread. */
    fun onTap() {
        val cookie = nextCookie++
        section.begin(BenchmarkAnchors.INPUT_TO_FRAME_SECTION, cookie)
        postFrame {
            postFrame {
                section.end(BenchmarkAnchors.INPUT_TO_FRAME_SECTION, cookie)
            }
        }
    }
}
