package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The hand-off from the surface to the store: a verdict is queued as one write, a move or a
 * refused verb queues nothing, and only the sweep's confirmation queues the sweep.
 */
class CullingViewModelTest {
    /** A stand-in for [WriteQueue.submit] that keeps what is submitted instead of running it. */
    private class RecordingQueue {
        val submitted = mutableListOf<suspend () -> Unit>()

        fun submit(write: suspend () -> Unit) {
            submitted += write
        }
    }

    private fun id(n: Int) = AssetId("0192f0c0-0000-7000-8000-%012d".format(n))

    private fun setup(vararg flags: CullFlag): Triple<InMemoryCullPort, RecordingQueue, CullingHandOff> {
        val port = InMemoryCullPort(flags.mapIndexed { i, flag -> CullAsset(id(i + 1), flag) })
        val queue = RecordingQueue()
        val model = CullingModel(port)
        runBlocking { model.load() }
        return Triple(port, queue, CullingHandOff(model, queue::submit))
    }

    @Test
    fun aVerdictQueuesOneWriteThatReachesTheStore() =
        runBlocking {
            val (port, queue, handOff) = setup(CullFlag.NEUTRAL, CullFlag.NEUTRAL)

            handOff.perform(CullVerb.REJECT)

            assertEquals(1, queue.submitted.size)
            assertEquals(CullFlag.NEUTRAL, port.assets[0].flag, "nothing is written until the queue runs")
            queue.submitted.single()()
            assertEquals(CullFlag.REJECT, port.assets[0].flag)
        }

    @Test
    fun aMoveQueuesNoWrite() {
        val (_, queue, handOff) = setup(CullFlag.NEUTRAL, CullFlag.NEUTRAL)

        handOff.perform(CullVerb.NEXT)

        assertTrue(queue.submitted.isEmpty())
    }

    @Test
    fun aVerbThePassRefusesQueuesNoWrite() {
        val (_, queue, handOff) = setup(CullFlag.NEUTRAL)

        handOff.perform(CullVerb.PREVIOUS)
        handOff.perform(CullVerb.SWEEP)

        assertTrue(queue.submitted.isEmpty(), "no rejects, so neither a move back nor a sweep does anything")
    }

    @Test
    fun theSweepKeyOnlyAsksAndConfirmingQueuesTheSweep() =
        runBlocking {
            val (port, queue, handOff) = setup(CullFlag.REJECT, CullFlag.NEUTRAL)

            handOff.perform(CullVerb.SWEEP)
            assertTrue(queue.submitted.isEmpty(), "asking for the sweep moves nothing")

            handOff.confirmSweep()
            assertEquals(1, queue.submitted.size)
            assertTrue(port.trashed.isEmpty())
            queue.submitted.single()()
            assertEquals(listOf(id(1)), port.trashed)
        }

    @Test
    fun writesQueueInTheOrderTheUserMadeThem() =
        runBlocking {
            val (port, queue, handOff) = setup(CullFlag.NEUTRAL)

            handOff.perform(CullVerb.REJECT)
            handOff.perform(CullVerb.PICK)

            assertEquals(2, queue.submitted.size)
            queue.submitted.forEach { it() }
            assertEquals(CullFlag.PICK, port.assets.single().flag)
        }
}
