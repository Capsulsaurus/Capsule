package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.StackId
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The culling model writes every verdict through the port, puts a flag back when its write
 * fails, reaches a whole stack through the store, and sweeps only after the confirmation,
 * moving exactly the pass's rejects.
 */
class CullingModelTest {
    @Test
    fun aLoadedPassShowsItsFirstAssetAndWhatItAccepts() =
        runBlocking {
            val model = CullingModel(InMemoryCullPort(listOf(asset(1), asset(2, CullFlag.REJECT))))

            assertEquals(CullingPhase.LOADING, model.state.value.phase)
            assertTrue(model.load())

            val state = model.state.value
            assertEquals(CullingPhase.READY, state.phase)
            assertEquals(id(1), state.asset)
            assertEquals(CullStepFlag.Asset(CullFlag.NEUTRAL), state.flag)
            assertEquals(CullPosition(1, 2), state.position)
            assertEquals(1, state.sweepCount)
            assertTrue(CullVerb.NEXT in state.accepted)
            assertFalse(CullVerb.PREVIOUS in state.accepted)
            assertTrue(CullVerb.SWEEP in state.accepted)
        }

    @Test
    fun aPassThatCannotBeReadFails() =
        runBlocking {
            val port = InMemoryCullPort(listOf(asset(1))).apply { failing = true }
            val model = CullingModel(port)

            assertFalse(model.load())

            assertEquals(CullingPhase.FAILED, model.state.value.phase)
            assertNull(model.state.value.asset)
        }

    @Test
    fun anEmptyPassIsReadyWithNothingToDo() =
        runBlocking {
            val model = CullingModel(InMemoryCullPort())

            model.load()

            assertEquals(CullingPhase.READY, model.state.value.phase)
            assertNull(model.state.value.asset)
            assertTrue(model.state.value.accepted.isEmpty())
        }

    @Test
    fun aVerdictShowsAtOnceAdvancesAndIsWritten() =
        runBlocking {
            val port = InMemoryCullPort(listOf(asset(1), asset(2)))
            val model = CullingModel(port).apply { load() }

            val verdict = model.perform(CullVerb.PICK)

            assertEquals(id(2), model.state.value.asset)
            model.write(verdict!!)
            assertEquals(CullFlag.PICK, port.assets.first { it.id == id(1) }.flag)
            model.perform(CullVerb.PREVIOUS)
            assertEquals(CullStepFlag.Asset(CullFlag.PICK), model.state.value.flag)
            assertNull(model.state.value.notice)
        }

    @Test
    fun aFailedWritePutsTheFlagBackAndSaysSo() =
        runBlocking {
            val port = InMemoryCullPort(listOf(asset(1, CullFlag.PICK), asset(2)))
            val model = CullingModel(port).apply { load() }

            val verdict = model.perform(CullVerb.REJECT)!!
            port.failing = true
            model.write(verdict)

            assertEquals(CullNotice.FlagFailed, model.state.value.notice)
            assertEquals(id(2), model.state.value.asset, "the user stays where the verdict sent them")
            assertEquals(0, model.state.value.sweepCount)
            model.consumeNotice()
            assertNull(model.state.value.notice)
        }

    @Test
    fun aStackVerdictReachesMembersThePassNeverRead() =
        runBlocking {
            val unread = asset(12, membership = StackMembership(STACK, memberIndex = 2))
            val port = InMemoryCullPort(stack(10, 11) + asset(1) + unread)
            val model = CullingModel(OnlyReads(port, setOf(unread.id))).apply { load() }

            val verdict = model.perform(CullVerb.REJECT)!!
            model.write(verdict)

            assertEquals(STACK, verdict.stack)
            assertTrue(port.assets.filter { it.stack != null }.all { it.flag == CullFlag.REJECT })
            assertEquals(2, model.state.value.sweepCount, "the pass sweeps only what it holds")
        }

    @Test
    fun keysThatDoNothingFromHereAreIgnored() =
        runBlocking {
            val model = CullingModel(InMemoryCullPort(listOf(asset(1)))).apply { load() }

            assertNull(model.perform(CullVerb.PREVIOUS))
            assertNull(model.perform(CullVerb.EXPAND_STACK))
            assertNull(model.perform(CullVerb.SWEEP))
            assertFalse(model.state.value.isConfirmingSweep, "nothing to sweep, so nothing to confirm")
        }

    @Test
    fun theSweepAsksFirstAndADismissedOneMovesNothing() =
        runBlocking {
            val port = InMemoryCullPort(listOf(asset(1, CullFlag.REJECT), asset(2)))
            val model = CullingModel(port).apply { load() }

            assertNull(model.perform(CullVerb.SWEEP))
            assertTrue(model.state.value.isConfirmingSweep)
            model.dismissSweep()

            assertFalse(model.state.value.isConfirmingSweep)
            assertTrue(port.trashed.isEmpty())
            assertEquals(2, model.state.value.position.count)
        }

    @Test
    fun aConfirmedSweepMovesExactlyThePassesRejects() =
        runBlocking {
            val outside = asset(9, CullFlag.REJECT)
            val port = InMemoryCullPort(listOf(asset(1, CullFlag.REJECT), asset(2), asset(3, CullFlag.REJECT), outside))
            val model = CullingModel(OnlyReads(port, setOf(outside.id))).apply { load() }
            model.perform(CullVerb.NEXT)

            model.perform(CullVerb.SWEEP)
            model.confirmSweep()

            assertEquals(listOf(id(1), id(3)), port.trashed)
            val state = model.state.value
            assertFalse(state.isConfirmingSweep)
            assertEquals(CullNotice.Swept(2), state.notice)
            assertEquals(id(2), state.asset, "the user stays on the asset they were looking at")
            assertEquals(CullPosition(1, 1), state.position)
            assertEquals(0, state.sweepCount)
        }

    @Test
    fun aSweepThatStopsPartwayDropsOnlyWhatItMoved() =
        runBlocking {
            val port = StopsAfterOne(InMemoryCullPort(listOf(asset(1, CullFlag.REJECT), asset(2, CullFlag.REJECT), asset(3))))
            val model = CullingModel(port).apply { load() }

            model.perform(CullVerb.SWEEP)
            model.confirmSweep()

            assertEquals(CullNotice.SweepFailed, model.state.value.notice)
            assertEquals(CullPosition(1, 2), model.state.value.position)
            assertEquals(1, model.state.value.sweepCount)
        }

    @Test
    fun aSweepThatFailsOutrightKeepsThePass() =
        runBlocking {
            val port = InMemoryCullPort(listOf(asset(1, CullFlag.REJECT)))
            val model = CullingModel(port).apply { load() }
            port.failing = true

            model.perform(CullVerb.SWEEP)
            model.confirmSweep()

            assertEquals(CullNotice.SweepFailed, model.state.value.notice)
            assertEquals(1, model.state.value.sweepCount)
        }

    /** A port whose pass leaves out [hidden], as a pass over part of the library would. */
    private class OnlyReads(
        private val port: InMemoryCullPort,
        private val hidden: Set<AssetId>,
    ) : CullPort by port {
        override suspend fun pass(): List<CullAsset> = port.pass().filterNot { it.id in hidden }
    }

    /** A port whose sweep stops after moving the first asset. */
    private class StopsAfterOne(
        private val port: InMemoryCullPort,
    ) : CullPort by port {
        override suspend fun moveToTrash(assets: List<AssetId>): List<AssetId> {
            val moved = port.moveToTrash(assets.take(1))
            throw SweepIncomplete(moved)
        }
    }

    private companion object {
        val STACK = StackId("0192f0c0-9999-7000-8000-000000000009")

        fun id(n: Int): AssetId = AssetId("0192f0c0-0000-7000-8000-%012d".format(n))

        fun asset(
            n: Int,
            flag: CullFlag = CullFlag.NEUTRAL,
            membership: StackMembership? = null,
        ): CullAsset = CullAsset(id(n), flag, membership)

        fun stack(vararg members: Int): List<CullAsset> =
            members.mapIndexed { offset, n -> asset(n, membership = StackMembership(STACK, isCover = offset == 0, memberIndex = offset)) }
    }
}
