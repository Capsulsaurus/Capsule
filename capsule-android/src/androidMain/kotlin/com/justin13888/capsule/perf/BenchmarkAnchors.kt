package com.justin13888.capsule.perf

/**
 * The names the responsiveness parity benchmark (`capsule-android/benchmark`) finds the app by.
 *
 * The benchmark is a separate APK and cannot import this module, so it repeats these values;
 * they are the contract, and a mismatch fails the benchmark loudly (it waits for an anchor that
 * never appears) rather than silently timing the wrong thing. The tags match the Apple client's
 * accessibility identifiers, so both platforms' harnesses speak of the same surfaces.
 *
 * None of these is a user-facing string: tags are resource ids, and the trace section is a
 * Perfetto slice name.
 */
object BenchmarkAnchors {
    /** A thumbnail in the library grid. */
    const val GRID_TILE = "grid.tile"

    /** The full-resolution image in the viewer. */
    const val VIEWER_IMAGE = "viewer.image"

    /** The trace section from a tap to the frame that answers it (see [InputToFrameTracer]). */
    const val INPUT_TO_FRAME_SECTION = "inputToFrame"
}
