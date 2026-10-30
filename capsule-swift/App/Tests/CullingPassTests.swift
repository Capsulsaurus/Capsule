import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
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

        let written = pass.verdict(.pick)

        #expect(written == [Fixture.id(1)])
        #expect(pass.currentAsset?.id == Fixture.id(2))
        #expect(pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .pick])
        #expect(pass.position == (2, 3))
    }

    @Test("a verdict on the last asset flags it and stays")
    func verdictAtTheEndStays() {
        var pass = CullingPass(assets: [Fixture.asset(1), Fixture.asset(2)])
        _ = pass.next()

        let written = pass.verdict(.reject)

        #expect(written == [Fixture.id(2)])
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
    func restoreUndoesAFailedWrite() {
        var pass = CullingPass(assets: [Fixture.asset(1, cull: .pick), Fixture.asset(2)])
        let before = pass.flags(of: [Fixture.id(1)])

        _ = pass.verdict(.reject)
        pass.restore(before)

        #expect(pass.flags(of: [Fixture.id(1)]) == [Fixture.id(1): .pick])
        #expect(pass.currentAsset?.id == Fixture.id(2))
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
        let written = pass.verdict(.pick)
        #expect(written.isEmpty)
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
        let targets = pass.verdictTargets

        let written = pass.verdict(.reject)

        #expect(written == targets)

        #expect(Set(written) == Set([Fixture.id(10), Fixture.id(11), Fixture.id(12)]))
        #expect(pass.currentAsset?.id == Fixture.id(1))
        _ = pass.previous()
        #expect(pass.currentFlag == .stack(.allRejected, memberCount: 3))
    }

    @Test("expanding steps through members from the cover, collapsing lands on the stack")
    func expandAndCollapse() {
        var pass = CullingPass(assets: Fixture.stack(members: [10, 11, 12]) + [Fixture.asset(1)])

        let expanded = pass.expandStack()
        #expect(expanded)
        #expect(pass.steps.count == 4)
        #expect(pass.currentAsset?.id == Fixture.id(10))
        #expect(pass.currentFlag == .asset(.neutral))

        let written = pass.verdict(.pick)
        #expect(written == [Fixture.id(10)])
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

private enum Fixture {
    static let stackID = StackID("0192f0c0-9999-7000-8000-000000000009")

    static func id(_ n: Int) -> AssetID {
        .managed(uuid: String(format: "0192f0c0-0000-7000-8000-%012d", n))
    }

    static func asset(_ n: Int, cull: CullFlag = .neutral, membership: StackMembership? = nil) -> LibraryAsset {
        LibraryAsset(
            id: id(n),
            mediaType: .photo,
            contentType: .jpeg,
            captureTime: CaptureTime(captureTimestamp: CapsuleTimestamp(epochSeconds: Int64(n))),
            importTimestamp: CapsuleTimestamp(epochSeconds: 0),
            cull: cull,
            stackMembership: membership
        )
    }

    /// A burst whose first member is the cover, numbered as given.
    static func stack(members: [Int], cull: CullFlag = .neutral) -> [LibraryAsset] {
        members.enumerated().map { offset, n in
            asset(n, cull: cull, membership: StackMembership(
                stackID: stackID,
                stackType: .burst,
                role: offset == 0 ? .primary : .member,
                memberIndex: UInt32(offset)
            ))
        }
    }
}
