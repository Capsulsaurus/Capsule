import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import CapsulePorts
import Foundation
import Testing

@testable import Capsule

/// The cull pass's rules, as values: every verb a key, a button or a VoiceOver
/// action reaches goes through ``CullingPass``, so these are the culling
/// surface's behaviour without a screen.
@Suite("The cull pass")
struct CullingPassTests {
    // MARK: Verdicts

    @Test("a verdict flags the asset and advances to the next")
    func verdictAdvances() {
        var pass = CullingPass(assets: [Fixture.asset(1), Fixture.asset(2), Fixture.asset(3)])

        let verdict = pass.verdict(.pick)

        #expect(verdict?.targets == [Fixture.id(1)])
        #expect(verdict?.stackID == nil)
        #expect(verdict?.writeTargets(stackMembers: [Fixture.id(9)]) == [Fixture.id(1)])
        #expect(pass.currentAsset?.id == Fixture.id(2))
        #expect(pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .pick])
        #expect(pass.position == (2, 3))
    }

    @Test("a verdict on the last asset flags it and stays")
    func verdictAtTheEndStays() {
        var pass = CullingPass(assets: [Fixture.asset(1), Fixture.asset(2)])
        _ = pass.next()

        let verdict = pass.verdict(.reject)

        #expect(verdict?.targets == [Fixture.id(2)])
        #expect(pass.currentAsset?.id == Fixture.id(2))
        #expect(pass.currentFlag == .asset(.reject))
    }

    @Test("neutral clears a flag")
    func neutralClears() {
        var pass = CullingPass(assets: [Fixture.asset(1, cull: .reject)])

        _ = pass.verdict(.neutral)

        #expect(pass.currentFlag == .asset(.neutral))
        #expect(pass.sweepCandidates.isEmpty)
    }

    @Test("a failed write is undone without moving the user")
    func revertUndoesAFailedWrite() throws {
        var pass = CullingPass(assets: [Fixture.asset(1, cull: .pick), Fixture.asset(2)])

        let verdict = try pass.give(.reject)
        pass.revert(verdict)

        #expect(pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .pick])
        #expect(pass.currentAsset?.id == Fixture.id(2))
    }

    @Test("a failed write does not undo a later verdict on the same asset that was written")
    func revertKeepsALaterWrittenVerdict() throws {
        var pass = CullingPass(assets: [Fixture.asset(1, cull: .pick), Fixture.asset(2)])
        let first = try pass.give(.reject)
        _ = pass.previous()
        let second = try pass.give(.neutral)

        pass.settle(second, written: second.targets)
        pass.revert(first)

        #expect(pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .neutral])
    }

    @Test("a failed write puts back the stored flag, not an earlier verdict's unwritten one")
    func revertRestoresTheStoredFlag() throws {
        var pass = CullingPass(assets: [Fixture.asset(1, cull: .pick), Fixture.asset(2)])
        let first = try pass.give(.reject)
        _ = pass.previous()
        let second = try pass.give(.neutral)

        pass.revert(first)
        #expect(pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .neutral], "the later verdict still shows")
        pass.revert(second)

        #expect(pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .pick])
    }

    @Test("a write settled after a later verdict leaves the later one showing")
    func lateSettleKeepsTheLaterVerdict() throws {
        var pass = CullingPass(assets: [Fixture.asset(1), Fixture.asset(2)])
        let first = try pass.give(.reject)
        _ = pass.previous()
        let second = try pass.give(.pick)

        pass.settle(second, written: second.targets)
        pass.settle(first, written: first.targets)
        #expect(pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .pick])

        _ = pass.previous()
        let third = try pass.give(.neutral)
        pass.revert(third)
        #expect(
            pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .pick],
            "the newer verdict's write is the stored flag, whichever landed last"
        )
    }

    // MARK: Paging

    @Test("next and previous stop at the ends")
    func pagingIsBounded() {
        var pass = CullingPass(assets: [Fixture.asset(1), Fixture.asset(2)])

        #expect(!pass.accepts(.previous))
        let backFromStart = pass.previous()
        let forward = pass.next()
        let pastEnd = pass.next()
        #expect(!pass.accepts(.next))
        let back = pass.previous()

        #expect(!backFromStart)
        #expect(forward)
        #expect(!pastEnd)
        #expect(back)
        #expect(pass.currentAsset?.id == Fixture.id(1))
    }

    @Test("an empty pass accepts nothing and writes nothing")
    func emptyPass() {
        var pass = CullingPass()

        #expect(pass.isEmpty)
        #expect(pass.position == (0, 0))
        #expect(CullVerb.allCases.allSatisfy { !pass.accepts($0) })
        #expect(pass.verdict(.pick) == nil)
        #expect(pass.currentFlag == nil)
    }

    // MARK: Stacks

    @Test("a collapsed stack is one step, shown by its cover, with a derived state")
    func collapsedStackIsOneStep() {
        let pass = CullingPass(assets: Fixture.stack(members: [10, 11, 12]) + [Fixture.asset(1)])

        #expect(pass.steps.count == 2)
        #expect(pass.currentAsset?.id == Fixture.id(10))
        #expect(pass.currentFlag == .stack(.mixed, memberCount: 3))
        #expect(pass.accepts(.expandStack))
        #expect(!pass.accepts(.collapseStack))
    }

    @Test("a verdict on a collapsed stack flags every member")
    func stackVerdictFlagsEveryMember() {
        var pass = CullingPass(assets: Fixture.stack(members: [10, 11, 12]) + [Fixture.asset(1)])

        let verdict = pass.verdict(.reject)

        #expect(verdict?.stackID == Fixture.stackID)
        #expect(Set(verdict?.targets ?? []) == Set([Fixture.id(10), Fixture.id(11), Fixture.id(12)]))
        #expect(pass.currentAsset?.id == Fixture.id(1))
        _ = pass.previous()
        #expect(pass.currentFlag == .stack(.allRejected, memberCount: 3))
    }

    @Test("a verdict on a collapsed stack writes the members the pass never loaded")
    func stackVerdictReachesUnloadedMembers() throws {
        let members = Fixture.stack(members: [10, 11, 12, 13])
        var pass = CullingPass(assets: [members[0], members[2], Fixture.asset(1)])

        let verdict = try pass.give(.pick)
        let written = verdict.writeTargets(stackMembers: members.map(\.id))

        #expect(written == members.map(\.id), "the stack port's list, the members outside the pass included")
        #expect(
            verdict.writeTargets(stackMembers: [Fixture.id(10)]) == [Fixture.id(10), Fixture.id(12)],
            "a member the pass holds is written even if the port did not list it"
        )

        pass.append([members[1]])
        #expect(pass.flags(of: [Fixture.id(11)]) == [Fixture.id(11): .pick], "a page read before the write shows the verdict")

        pass.settle(verdict, written: written)
        pass.append([members[3]])
        #expect(pass.groups[0].members.allSatisfy { $0.cull == .pick })
    }

    @Test("a failed stack write is undone on members that arrived while it was in flight")
    func stackRevertReachesLateMembers() throws {
        let members = Fixture.stack(members: [10, 11], cull: .pick)
        var pass = CullingPass(assets: [members[0], Fixture.asset(1)])
        let verdict = try pass.give(.reject)
        pass.append([members[1]])

        pass.revert(verdict)

        #expect(pass.groups[0].members.allSatisfy { $0.cull == .pick })
        let late = CullStackPlace(stackID: Fixture.stackID, isCover: false, memberIndex: 2)
        pass.append([Fixture.asset(12, cull: .pick, place: late)])
        #expect(pass.flags(of: [Fixture.id(12)]) == [Fixture.id(12): .pick], "the reverted verdict is not adopted")
    }

    @Test("a member loaded after a write it was part of shows the written flag")
    func settleReachesMembersLoadedMeanwhile() throws {
        let members = Fixture.stack(members: [10, 11])
        var pass = CullingPass(assets: [members[0], Fixture.asset(1)])
        let verdict = try pass.give(.reject)

        pass.settle(verdict, written: members.map(\.id))
        pass.append([members[1]])

        #expect(pass.flags(of: [Fixture.id(11)]) == [Fixture.id(11): .reject])
    }

    @Test("expanding steps through members from the cover, collapsing lands on the stack")
    func expandAndCollapse() {
        var pass = CullingPass(assets: Fixture.stack(members: [10, 11, 12]) + [Fixture.asset(1)])

        let expanded = pass.expandStack()
        #expect(expanded)
        #expect(pass.steps.count == 4)
        #expect(pass.currentAsset?.id == Fixture.id(10))
        #expect(pass.currentFlag == .asset(.neutral))

        let verdict = pass.verdict(.pick)
        #expect(verdict?.targets == [Fixture.id(10)])
        #expect(verdict?.stackID == nil, "a member of an expanded stack is flagged alone")
        #expect(pass.currentAsset?.id == Fixture.id(11))
        #expect(pass.accepts(.collapseStack))

        let collapsed = pass.collapseStack()
        #expect(collapsed)
        #expect(pass.steps.count == 2)
        #expect(pass.index == 0)
        #expect(pass.currentFlag == .stack(.anyPick, memberCount: 3))
    }

    @Test("a lone asset and a one-member stack cannot be expanded")
    func nothingToExpand() {
        var pass = CullingPass(assets: [Fixture.asset(1)] + Fixture.stack(members: [10]))

        let expanded = pass.expandStack()
        #expect(!expanded)
        _ = pass.next()
        #expect(!pass.accepts(.expandStack))
        #expect(pass.currentFlag == .asset(.neutral))
    }

    @Test("members order cover first, then by member index, whatever order they arrive in")
    func memberOrder() {
        let members = Fixture.stack(members: [10, 11, 12])
        let pass = CullingPass(assets: [members[2], members[1], members[0]])

        #expect(pass.groups.count == 1)
        #expect(pass.groups[0].members.map(\.id) == [Fixture.id(10), Fixture.id(11), Fixture.id(12)])
    }

    @Test("a stack split across pages is still one step, and the user stays put")
    func stackAcrossPages() {
        let members = Fixture.stack(members: [10, 11, 12])
        var pass = CullingPass(assets: [Fixture.asset(1), members[1]])
        _ = pass.next()

        pass.append([Fixture.asset(2), members[0], members[2]])

        #expect(pass.steps.count == 3)
        #expect(pass.index == 1)
        #expect(pass.currentAsset?.id == Fixture.id(10), "the cover arrived and now fronts the stack")
        #expect(pass.currentFlag == .stack(.mixed, memberCount: 3))
    }

    // MARK: Sweep

    @Test("the sweep takes exactly the pass's rejects, and they leave the pass")
    func sweep() {
        var pass = CullingPass(
            assets: [Fixture.asset(1, cull: .reject), Fixture.asset(2, cull: .pick)]
                + Fixture.stack(members: [10, 11], cull: .reject)
                + [Fixture.asset(3)]
        )
        _ = pass.next()

        #expect(pass.accepts(.sweep))
        let swept = pass.sweepCandidates
        #expect(swept == [Fixture.id(1), Fixture.id(10), Fixture.id(11)])

        pass.remove(Set(swept))

        #expect(pass.steps.count == 2)
        #expect(pass.currentAsset?.id == Fixture.id(2), "the user stays on the asset they were looking at")
        #expect(!pass.accepts(.sweep))
    }

    @Test("sweeping the asset under review lands on the step that took its place")
    func sweepUnderTheCursor() {
        var pass = CullingPass(assets: [Fixture.asset(1), Fixture.asset(2, cull: .reject)])
        _ = pass.next()

        pass.remove([Fixture.id(2)])

        #expect(pass.currentAsset?.id == Fixture.id(1))
        #expect(pass.position == (1, 1))
    }

    // MARK: Wiring

    @Test("every action in the cull pass's shortcut list reaches a verb, and nothing else does")
    func everyPassCommandHasAVerb() {
        let verbs = NavigationCommand.cullingPass.compactMap { CullVerb(action: $0.action) }

        #expect(Set(verbs) == Set(CullVerb.allCases))
        #expect(verbs.count == NavigationCommand.cullingPass.count)
        #expect(CullVerb(action: .importMedia) == nil)
        #expect(CullVerb(action: .cullingReview) == nil)
    }

    @Test("only the three verdicts carry a flag")
    func verdictFlags() {
        #expect(CullVerb.allCases.compactMap(\.flag) == [.pick, .neutral, .reject])
    }

    @Test("a pass reads the timeline or an album, always with stack members")
    func passQueries() {
        let albumID = AlbumID.managed(uuid: "0192f0c0-2222-7000-8000-000000000002")

        #expect(CullingPass.query(for: .library) == TimelineQuery(includeStackHidden: true))
        #expect(
            CullingPass.query(for: .timeline(TimelineQuery(cull: .reject)))
                == TimelineQuery(cull: .reject, includeStackHidden: true)
        )
        #expect(CullingPass.query(for: .album(albumID)) == TimelineQuery(albumID: albumID, includeStackHidden: true))
        #expect(CullingPass.query(for: .person(PersonID("p"))) == nil)
    }

    @Test("shortcuts print in the platform's notation")
    func shortcutText() {
        #expect(CommandShortcut("p", modifiers: []).displayText == "P")
        #expect(CommandShortcut(.delete).displayText == "⌘⌫")
        #expect(CommandShortcut(.rightArrow, modifiers: []).displayText == "→")
        #expect(CommandShortcut("r", modifiers: [.command, .shift]).displayText == "⇧⌘R")
    }
}

// MARK: - Fixtures

private extension CullingPass {
    /// A verdict on a pass that is not empty.
    mutating func give(_ flag: CullFlag) throws -> CullVerdict {
        let given = verdict(flag)
        return try #require(given)
    }
}

private enum Fixture {
    static let stackID = StackID("0192f0c0-9999-7000-8000-000000000009")

    static func id(_ n: Int) -> AssetID {
        .managed(uuid: String(format: "0192f0c0-0000-7000-8000-%012d", n))
    }

    static func asset(_ n: Int, cull: CullFlag = .neutral, place: CullStackPlace? = nil) -> CullAsset {
        CullAsset(id: id(n), cull: cull, stack: place)
    }

    /// A burst whose first member is the cover, numbered as given.
    static func stack(members: [Int], cull: CullFlag = .neutral) -> [CullAsset] {
        members.enumerated().map { offset, n in
            asset(n, cull: cull, place: CullStackPlace(
                stackID: stackID,
                isCover: offset == 0,
                memberIndex: UInt32(offset)
            ))
        }
    }
}
