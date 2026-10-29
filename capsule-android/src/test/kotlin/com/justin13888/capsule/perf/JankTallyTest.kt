package com.justin13888.capsule.perf

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class JankTallyTest {
    @Test
    fun reportsEachFullWindowAndResets() {
        val reports = mutableListOf<JankSummary>()
        val tally = JankTally(windowFrames = 3) { reports += it }
        tally.record(8_000_000, isJank = false)
        tally.record(40_000_000, isJank = true)
        assertTrue(reports.isEmpty(), "a partial window is not reported until flushed")
        tally.record(10_000_000, isJank = false)
        tally.record(9_000_000, isJank = false)
        assertEquals(listOf(JankSummary(frames = 3, jankyFrames = 1, worstFrameNanos = 40_000_000)), reports)
    }

    @Test
    fun aCleanWindowIsStillReported() {
        val reports = mutableListOf<JankSummary>()
        val tally = JankTally(windowFrames = 2) { reports += it }
        tally.record(8_000_000, isJank = false)
        tally.record(8_000_000, isJank = false)
        assertEquals(0.0, reports.single().jankyPercent)
    }

    @Test
    fun flushReportsAPartialWindowOnceAndAnEmptyOneNever() {
        val reports = mutableListOf<JankSummary>()
        val tally = JankTally(windowFrames = 10) { reports += it }
        tally.flush()
        assertTrue(reports.isEmpty())
        tally.record(20_000_000, isJank = true)
        tally.flush()
        tally.flush()
        assertEquals(listOf(JankSummary(frames = 1, jankyFrames = 1, worstFrameNanos = 20_000_000)), reports)
        assertEquals(100.0, reports.single().jankyPercent)
    }

    @Test
    fun anEmptyWindowSizeIsRefused() {
        assertFailsWith<IllegalArgumentException> { JankTally(windowFrames = 0) {} }
    }
}
