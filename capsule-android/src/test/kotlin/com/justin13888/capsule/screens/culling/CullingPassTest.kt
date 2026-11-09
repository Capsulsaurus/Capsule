package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.StackId
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The cull pass's rules, as values: every verb a key, a button or a TalkBack action reaches
 * goes through [CullingPass], so these are the culling surface's behaviour without a screen.
 * The same cases as the Apple client's `CullingPassTests`.
 */
class CullingPassTest {
    // Verdicts

    @Test
    fun aVerdictFlagsTheAssetAndAdvancesToTheNext() {
        val pass = CullingPass(listOf(asset(1), asset(2), asset(3)))

        val verdict = pass.give(CullFlag.PICK)

        assertEquals(listOf(id(1)), verdict.targets)
        assertNull(verdict.stack)
        assertEquals(listOf(id(1)), verdict.writeTargets(listOf(id(9))))
        assertEquals(id(2), pass.currentAsset?.id)
        assertEquals(mapOf(id(1) to CullFlag.PICK), pass.flags(listOf(id(1))))
        assertEquals(CullPosition(2, 3), pass.position)
    }

    @Test
    fun aVerdictOnTheLastAssetFlagsItAndStays() {
        val pass = CullingPass(listOf(asset(1), asset(2)))
        pass.next()

        val verdict = pass.give(CullFlag.REJECT)

        assertEquals(listOf(id(2)), verdict.targets)
        assertEquals(id(2), pass.currentAsset?.id)
        assertEquals(CullStepFlag.Asset(CullFlag.REJECT), pass.currentFlag)
    }

    @Test
    fun neutralClearsAFlag() {
        val pass = CullingPass(listOf(asset(1, CullFlag.REJECT)))

        pass.give(CullFlag.NEUTRAL)

        assertEquals(CullStepFlag.Asset(CullFlag.NEUTRAL), pass.currentFlag)
        assertTrue(pass.sweepCandidates.isEmpty())
    }

    @Test
    fun aFailedWriteIsUndoneWithoutMovingTheUser() {
        val pass = CullingPass(listOf(asset(1, CullFlag.PICK), asset(2)))

        val verdict = pass.give(CullFlag.REJECT)
        pass.revert(verdict)

        assertEquals(mapOf(id(1) to CullFlag.PICK), pass.flags(listOf(id(1))))
        assertEquals(id(2), pass.currentAsset?.id)
    }

    @Test
    fun aFailedWriteDoesNotUndoALaterVerdictThatWasWritten() {
        val pass = CullingPass(listOf(asset(1, CullFlag.PICK), asset(2)))
        val first = pass.give(CullFlag.REJECT)
        pass.previous()
        val second = pass.give(CullFlag.NEUTRAL)

        pass.settle(second, second.targets)
        pass.revert(first)

        assertEquals(mapOf(id(1) to CullFlag.NEUTRAL), pass.flags(listOf(id(1))))
    }

    @Test
    fun aFailedWritePutsBackTheStoredFlagNotAnEarlierUnwrittenOne() {
        val pass = CullingPass(listOf(asset(1, CullFlag.PICK), asset(2)))
        val first = pass.give(CullFlag.REJECT)
        pass.previous()
        val second = pass.give(CullFlag.NEUTRAL)

        pass.revert(first)
        assertEquals(mapOf(id(1) to CullFlag.NEUTRAL), pass.flags(listOf(id(1))), "the later verdict still shows")
        pass.revert(second)

        assertEquals(mapOf(id(1) to CullFlag.PICK), pass.flags(listOf(id(1))))
    }

    @Test
    fun aWriteSettledAfterALaterVerdictLeavesTheLaterOneShowing() {
        val pass = CullingPass(listOf(asset(1), asset(2)))
        val first = pass.give(CullFlag.REJECT)
        pass.previous()
        val second = pass.give(CullFlag.PICK)

        pass.settle(second, second.targets)
        pass.settle(first, first.targets)
        assertEquals(mapOf(id(1) to CullFlag.PICK), pass.flags(listOf(id(1))))

        pass.previous()
        val third = pass.give(CullFlag.NEUTRAL)
        pass.revert(third)
        assertEquals(
            mapOf(id(1) to CullFlag.PICK),
            pass.flags(listOf(id(1))),
            "the newer verdict's write is the stored flag, whichever landed last",
        )
    }

    // Paging

    @Test
    fun nextAndPreviousStopAtTheEnds() {
        val pass = CullingPass(listOf(asset(1), asset(2)))

        assertFalse(pass.accepts(CullVerb.PREVIOUS))
        assertFalse(pass.previous())
        assertTrue(pass.next())
        assertFalse(pass.next())
        assertFalse(pass.accepts(CullVerb.NEXT))
        assertTrue(pass.previous())
        assertEquals(id(1), pass.currentAsset?.id)
    }

    @Test
    fun anEmptyPassAcceptsNothingAndWritesNothing() {
        val pass = CullingPass()

        assertTrue(pass.isEmpty)
        assertEquals(CullPosition(0, 0), pass.position)
        assertTrue(CullVerb.entries.none(pass::accepts))
        assertNull(pass.verdict(CullFlag.PICK))
        assertNull(pass.currentFlag)
    }

    // Stacks

    @Test
    fun aCollapsedStackIsOneStepShownByItsCoverWithADerivedState() {
        val pass = CullingPass(stack(10, 11, 12) + asset(1))

        assertEquals(2, pass.steps.size)
        assertEquals(id(10), pass.currentAsset?.id)
        assertEquals(CullStepFlag.Stack(StackCullState.MIXED, 3), pass.currentFlag)
        assertTrue(pass.accepts(CullVerb.EXPAND_STACK))
        assertFalse(pass.accepts(CullVerb.COLLAPSE_STACK))
    }

    @Test
    fun aVerdictOnACollapsedStackFlagsEveryMember() {
        val pass = CullingPass(stack(10, 11, 12) + asset(1))

        val verdict = pass.give(CullFlag.REJECT)

        assertEquals(STACK, verdict.stack)
        assertEquals(setOf(id(10), id(11), id(12)), verdict.targets.toSet())
        assertEquals(id(1), pass.currentAsset?.id)
        pass.previous()
        assertEquals(CullStepFlag.Stack(StackCullState.ALL_REJECTED, 3), pass.currentFlag)
    }

    @Test
    fun aVerdictOnACollapsedStackWritesTheMembersThePassNeverLoaded() {
        val members = stack(10, 11, 12, 13)
        val pass = CullingPass(listOf(members[0], members[2], asset(1)))

        val verdict = pass.give(CullFlag.PICK)
        val written = verdict.writeTargets(members.map { it.id })

        assertEquals(members.map { it.id }, written, "the store's list, the members outside the pass included")
        assertEquals(
            listOf(id(10), id(12)),
            verdict.writeTargets(listOf(id(10))),
            "a member the pass holds is written even if the store did not list it",
        )

        pass.append(listOf(members[1]))
        assertEquals(mapOf(id(11) to CullFlag.PICK), pass.flags(listOf(id(11))), "a page read before the write shows the verdict")

        pass.settle(verdict, written)
        pass.append(listOf(members[3]))
        assertTrue(pass.groups[0].members.all { it.flag == CullFlag.PICK })
    }

    @Test
    fun aFailedStackWriteIsUndoneOnMembersThatArrivedWhileItWasInFlight() {
        val members = stack(10, 11, flag = CullFlag.PICK)
        val pass = CullingPass(listOf(members[0], asset(1)))
        val verdict = pass.give(CullFlag.REJECT)
        pass.append(listOf(members[1]))

        pass.revert(verdict)

        assertTrue(pass.groups[0].members.all { it.flag == CullFlag.PICK })
        pass.append(listOf(asset(12, CullFlag.PICK, StackMembership(STACK, memberIndex = 2))))
        assertEquals(mapOf(id(12) to CullFlag.PICK), pass.flags(listOf(id(12))), "the reverted verdict is not adopted")
    }

    @Test
    fun aMemberLoadedAfterAWriteItWasPartOfShowsTheWrittenFlag() {
        val members = stack(10, 11)
        val pass = CullingPass(listOf(members[0], asset(1)))
        val verdict = pass.give(CullFlag.REJECT)

        pass.settle(verdict, members.map { it.id })
        pass.append(listOf(members[1]))

        assertEquals(mapOf(id(11) to CullFlag.REJECT), pass.flags(listOf(id(11))))
    }

    @Test
    fun expandingStepsThroughMembersFromTheCoverAndCollapsingLandsOnTheStack() {
        val pass = CullingPass(stack(10, 11, 12) + asset(1))

        assertTrue(pass.expandStack())
        assertEquals(4, pass.steps.size)
        assertEquals(id(10), pass.currentAsset?.id)
        assertEquals(CullStepFlag.Asset(CullFlag.NEUTRAL), pass.currentFlag)

        val verdict = pass.give(CullFlag.PICK)
        assertEquals(listOf(id(10)), verdict.targets)
        assertNull(verdict.stack, "a member of an expanded stack is flagged alone")
        assertEquals(id(11), pass.currentAsset?.id)
        assertTrue(pass.accepts(CullVerb.COLLAPSE_STACK))

        assertTrue(pass.collapseStack())
        assertEquals(2, pass.steps.size)
        assertEquals(0, pass.index)
        assertEquals(CullStepFlag.Stack(StackCullState.ANY_PICK, 3), pass.currentFlag)
    }

    @Test
    fun aLoneAssetAndAOneMemberStackCannotBeExpanded() {
        val pass = CullingPass(listOf(asset(1)) + stack(10))

        assertFalse(pass.expandStack())
        pass.next()
        assertFalse(pass.accepts(CullVerb.EXPAND_STACK))
        assertEquals(CullStepFlag.Asset(CullFlag.NEUTRAL), pass.currentFlag)
    }

    @Test
    fun membersOrderCoverFirstThenByMemberIndexWhateverOrderTheyArriveIn() {
        val members = stack(10, 11, 12)
        val pass = CullingPass(listOf(members[2], members[1], members[0]))

        assertEquals(1, pass.groups.size)
        assertEquals(listOf(id(10), id(11), id(12)), pass.groups[0].members.map { it.id })
    }

    @Test
    fun aStackSplitAcrossPagesIsStillOneStepAndTheUserStaysPut() {
        val members = stack(10, 11, 12)
        val pass = CullingPass(listOf(asset(1), members[1]))
        pass.next()

        pass.append(listOf(asset(2), members[0], members[2]))

        assertEquals(3, pass.steps.size)
        assertEquals(1, pass.index)
        assertEquals(id(10), pass.currentAsset?.id, "the cover arrived and now fronts the stack")
        assertEquals(CullStepFlag.Stack(StackCullState.MIXED, 3), pass.currentFlag)
    }

    // Sweep

    @Test
    fun theSweepTakesExactlyThePassesRejectsAndTheyLeaveThePass() {
        val pass =
            CullingPass(
                listOf(asset(1, CullFlag.REJECT), asset(2, CullFlag.PICK)) + stack(10, 11, flag = CullFlag.REJECT) + asset(3),
            )
        pass.next()

        assertTrue(pass.accepts(CullVerb.SWEEP))
        val swept = pass.sweepCandidates
        assertEquals(listOf(id(1), id(10), id(11)), swept)

        pass.remove(swept.toSet())

        assertEquals(2, pass.steps.size)
        assertEquals(id(2), pass.currentAsset?.id, "the user stays on the asset they were looking at")
        assertFalse(pass.accepts(CullVerb.SWEEP))
    }

    @Test
    fun sweepingTheAssetUnderReviewLandsOnTheStepThatTookItsPlace() {
        val pass = CullingPass(listOf(asset(1), asset(2, CullFlag.REJECT)))
        pass.next()

        pass.remove(setOf(id(2)))

        assertEquals(id(1), pass.currentAsset?.id)
        assertEquals(CullPosition(1, 1), pass.position)
    }

    // Derived state and verbs

    @Test
    fun aStacksStateIsAllRejectedThenAnyPickElseMixed() {
        assertEquals(StackCullState.ALL_REJECTED, StackCullState.of(listOf(CullFlag.REJECT, CullFlag.REJECT)))
        assertEquals(StackCullState.ANY_PICK, StackCullState.of(listOf(CullFlag.REJECT, CullFlag.PICK)))
        assertEquals(StackCullState.MIXED, StackCullState.of(listOf(CullFlag.REJECT, CullFlag.NEUTRAL)))
        assertEquals(StackCullState.MIXED, StackCullState.of(emptyList()))
    }

    @Test
    fun onlyTheThreeVerdictsCarryAFlag() {
        assertEquals(listOf(CullFlag.PICK, CullFlag.NEUTRAL, CullFlag.REJECT), CullVerb.entries.mapNotNull { it.flag })
    }

    // Fixtures

    private fun CullingPass.give(flag: CullFlag): CullVerdict = assertNotNull(verdict(flag))

    private companion object {
        val STACK = StackId("0192f0c0-9999-7000-8000-000000000009")

        fun id(n: Int): AssetId = AssetId("0192f0c0-0000-7000-8000-%012d".format(n))

        fun asset(
            n: Int,
            flag: CullFlag = CullFlag.NEUTRAL,
            membership: StackMembership? = null,
        ): CullAsset = CullAsset(id(n), flag, membership)

        /** A burst whose first member is the cover, numbered as given. */
        fun stack(
            vararg members: Int,
            flag: CullFlag = CullFlag.NEUTRAL,
        ): List<CullAsset> =
            members.mapIndexed { offset, n ->
                asset(n, flag, StackMembership(STACK, isCover = offset == 0, memberIndex = offset))
            }
    }
}
