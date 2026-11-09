package com.justin13888.capsule.perf

import kotlin.test.Test
import kotlin.test.assertEquals

class InputToFrameTracerTest {
    private val events = mutableListOf<String>()
    private val pending = ArrayDeque<() -> Unit>()

    private val section =
        object : AsyncSection {
            override fun begin(
                name: String,
                cookie: Int,
            ) {
                events += "begin $name $cookie"
            }

            override fun end(
                name: String,
                cookie: Int,
            ) {
                events += "end $name $cookie"
            }
        }

    /** Run the callbacks due at the next frame, as Choreographer would. */
    private fun frame() {
        val due = pending.toList()
        pending.clear()
        due.forEach { it() }
    }

    private val tracer = InputToFrameTracer(section) { pending += it }

    @Test
    fun theSectionClosesOnlyAfterTheAnsweringFrame() {
        tracer.onTap()
        assertEquals(listOf("begin inputToFrame 0"), events)
        frame() // the frame that answers the tap: still open while it draws
        assertEquals(listOf("begin inputToFrame 0"), events)
        frame() // the next frame starts: the answer has been drawn
        assertEquals(listOf("begin inputToFrame 0", "end inputToFrame 0"), events)
    }

    @Test
    fun overlappingTapsAreMatchedByTheirOwnCookies() {
        tracer.onTap()
        frame()
        tracer.onTap()
        frame()
        frame()
        assertEquals(
            listOf("begin inputToFrame 0", "begin inputToFrame 1", "end inputToFrame 0", "end inputToFrame 1"),
            events,
        )
    }
}
