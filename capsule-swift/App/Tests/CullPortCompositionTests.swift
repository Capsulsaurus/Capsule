import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import Foundation
import Testing

@testable import Capsule

/// Stack places as the pass reads them: projected from a library row in the
/// mock lane, listed by the workspace in the FFI lane, and grouped the same
/// way whichever produced them.
@Suite("Cull stack places")
struct CullStackPlaceTests {
    @Test("a library row's stack membership becomes its place: the primary is the cover")
    func rowProjection() {
        let cover = Places.row(1, cull: .pick, membership: StackMembership(
            stackID: Places.stackID, stackType: .burst, role: .primary, memberIndex: 0
        ))
        let member = Places.row(2, membership: StackMembership(
            stackID: Places.stackID, stackType: .rawJpeg, role: .member
        ))

        let coverPlace = CullStackPlace(stackID: Places.stackID, isCover: true, memberIndex: 0)
        #expect(CullAsset(cover) == CullAsset(id: Places.id(1), cull: .pick, stack: coverPlace))
        let memberPlace = CullStackPlace(stackID: Places.stackID, isCover: false)
        #expect(CullAsset(member) == CullAsset(id: Places.id(2), stack: memberPlace))
        #expect(CullAsset(Places.row(3)) == CullAsset(id: Places.id(3)))
    }

    @Test("stacked assets arriving out of order group under their cover, and unlisted ones stay lone")
    func placesGroupTheStack() {
        let place = { (cover: Bool, index: UInt32?) in
            CullStackPlace(stackID: Places.stackID, isCover: cover, memberIndex: index)
        }
        // Id order, as the workspace reads the pass: the cover is not first.
        let pass = CullingPass(assets: [
            CullAsset(id: Places.id(1)),
            CullAsset(id: Places.id(2), stack: place(false, 2)),
            CullAsset(id: Places.id(3), stack: place(false, nil)),
            CullAsset(id: Places.id(4), stack: place(true, nil)),
            CullAsset(id: Places.id(5), stack: place(false, 1)),
        ])

        #expect(pass.steps.count == 2)
        #expect(pass.groups[0].stackID == nil)
        #expect(pass.groups[1].members.map(\.id) == [4, 5, 2, 3].map(Places.id))
        #expect(pass.groups[1].cover.id == Places.id(4))
    }
}

/// The mock lane's ``CullPort``: the pass and the writes go through the
/// library, organize and stack ports the rest of the mock world reads.
@MainActor
@Suite("The mock lane's cull port")
struct PortBackedCullPortTests {
    private let mock = MockEnvironment(scenario: .healthy)

    private var port: PortBackedCullPort {
        PortBackedCullPort(library: mock.library, organize: mock.organize, stacks: mock.stacks)
    }

    @Test("the pass is the library's query, a page per batch, projected to stack places")
    func passPagesTheLibrary() async throws {
        let query = TimelineQuery(includeStackHidden: true)
        var batches: [[CullAsset]] = []
        for try await batch in port.pass(over: query, batchSize: 50) {
            batches.append(batch)
        }

        let total = try await mock.library.assetCount(matching: query)
        let rows = try await mock.library.assets(matching: query, offset: 0, limit: total).items
        #expect(!batches.isEmpty)
        #expect(batches.allSatisfy { !$0.isEmpty && $0.count <= 50 })
        #expect(batches.flatMap { $0 } == rows.map(CullAsset.init))
    }

    @Test("flagging a stack flags every member the stack port lists, and answers them")
    func flagStackWritesEveryMember() async throws {
        let query = TimelineQuery(includeStackHidden: true)
        let rows = try await mock.library.assets(matching: query, offset: 0, limit: 500).items
        let stackID = try #require(rows.lazy.compactMap(\.stackMembership?.stackID).first)
        let listed = try await mock.stacks.members(of: stackID).map(\.id)

        let flagged = try await port.flagStack(stackID, as: .reject)

        #expect(flagged == listed)
        let members = try await mock.stacks.members(of: stackID)
        #expect(members.count == listed.count)
        #expect(members.allSatisfy { $0.cull == .reject })
    }

    @Test("a library page that fails ends the pass with its error, after the pages before it")
    func failedPageEndsThePass() async {
        let port = PortBackedCullPort(library: FailingPageLibrary(), organize: mock.organize, stacks: mock.stacks)
        var batches: [[CullAsset]] = []
        var failure: (any Error)?
        do {
            for try await batch in port.pass(over: .default, batchSize: 2) {
                batches.append(batch)
            }
        } catch {
            failure = error
        }

        #expect(failure is FailingPageLibrary.PageFailure)
        #expect(batches == [[CullAsset(id: Places.id(1)), CullAsset(id: Places.id(2))]])
    }

    @Test("an asset flag and the sweep reach the library")
    func flagAndSweep() async throws {
        let rows = try await mock.library.assets(matching: .default, offset: 0, limit: 2).items
        let ids = rows.map(\.id)
        try #require(ids.count == 2)

        try await port.setCull(.reject, for: ids)
        let flagged = try await mock.library.assets(for: ids)
        #expect(flagged.map(\.id) == ids)
        #expect(flagged.allSatisfy { $0.cull == .reject })

        try await port.moveToTrash(ids)
        let trashed = try await mock.library.assets(for: ids)
        #expect(trashed.map(\.id) == ids)
        #expect(trashed.allSatisfy(\.isDeleted))
    }

    @Test("the mock lane composes the port-backed pass")
    func mockLaneComposesIt() {
        let environment = AppEnvironment(mock: mock)
        #expect(environment.cull is PortBackedCullPort)
    }
}

/// What a sweep that stopped partway leaves in the pass.
@Suite("A partial sweep")
struct CullSweepIncompleteTests {
    private struct Failure: Error {}

    @Test("the assets an incomplete sweep moved leave the pass; the rest stay")
    func movedAssetsLeaveThePass() {
        var pass = CullingPass(assets: (1 ... 3).map { CullAsset(id: Places.id($0), cull: .reject) })
        let incomplete = CullSweepIncomplete(moved: [Places.id(1)], underlying: Failure())

        let moved = CullSweep.moved(before: incomplete)
        pass.remove(Set(moved))

        #expect(moved == [Places.id(1)])
        #expect(pass.sweepCandidates == [Places.id(2), Places.id(3)])
    }

    @Test("any other sweep failure names nothing moved")
    func otherFailureMovesNothing() {
        #expect(CullSweep.moved(before: Failure()).isEmpty)
    }
}

// MARK: - Fixtures

/// A library whose first page answers with `limit` rows and whose every later
/// page fails.
private struct FailingPageLibrary: LibraryPort {
    struct PageFailure: Error {}

    func assets(matching _: TimelineQuery, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        guard offset == 0 else { throw PageFailure() }
        return Page(items: (1 ... max(1, limit)).map { Places.row($0) }, request: PageRequest(offset: offset, limit: limit))
    }

    func dayCounts(matching _: TimelineQuery) async throws -> [DayCount] { [] }
    func assetCount(matching _: TimelineQuery) async throws -> Int { 0 }
    func asset(for _: AssetID) async throws -> LibraryAsset? { nil }
    func assets(for _: [AssetID]) async throws -> [LibraryAsset] { [] }
    func sidecar(for _: AssetID) async throws -> SidecarV1? { nil }
    func provenanceChain(for _: AssetID) async throws -> [ProvenanceRecord] { [] }
    func changes() -> AsyncStream<LibraryChange> { AsyncStream { $0.finish() } }
}

private enum Places {
    static let stackID = StackID("0192f0c0-9999-7000-8000-000000000009")

    static func id(_ number: Int) -> AssetID {
        .managed(uuid: String(format: "0192f0c0-0000-7000-8000-%012d", number))
    }

    /// A library row carrying `membership`.
    static func row(_ number: Int, cull: CullFlag = .neutral, membership: StackMembership? = nil) -> LibraryAsset {
        LibraryAsset(
            id: id(number),
            mediaType: .photo,
            contentType: .jpeg,
            captureTime: CaptureTime(captureTimestamp: CapsuleTimestamp(epochSeconds: Int64(number))),
            importTimestamp: CapsuleTimestamp(epochSeconds: 0),
            cull: cull,
            stackMembership: membership
        )
    }
}
