import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Testing

@testable import CapsuleCatalogFFI

/// A workspace double: holds each asset's flag and stack place, records every
/// write it receives, and throws `failure` from every verb when one is set, or
/// from `deleteAsset` once `deleteFailsAt` deletes have succeeded.
private final class RecordingWorkspace: FFICullWorkspace, @unchecked Sendable {
    enum Call: Equatable {
        case assetsByCull(FfiCullFlag)
        case stackMemberships
        case setCull(String, FfiCullFlag)
        case flagStack(String, FfiCullFlag)
        case deleteAsset(String, UInt32)
    }

    private let lock = NSLock()
    private var recorded: [Call] = []
    var failure: (any Error)?
    var deleteFailsAt: Int?
    var flags: [String: FfiCullFlag] = [:]
    var members: [FfiStackMember] = []
    var stackAnswer: [String] = []

    var calls: [Call] {
        lock.withLock { recorded }
    }

    private func record(_ call: Call) throws {
        try lock.withLock {
            recorded.append(call)
            if let failure { throw failure }
        }
    }

    func assetsByCull(flag: FfiCullFlag) throws -> [String] {
        try record(.assetsByCull(flag))
        return flags.filter { $0.value == flag }.keys.sorted()
    }

    func stackMemberships() throws -> [FfiStackMember] {
        try record(.stackMemberships)
        return members
    }

    func setCull(assetId: String, flag: FfiCullFlag) throws {
        try record(.setCull(assetId, flag))
    }

    func flagStack(stackId: String, flag: FfiCullFlag) throws -> [String] {
        try record(.flagStack(stackId, flag))
        return stackAnswer
    }

    func deleteAsset(assetId: String, retainDays: UInt32) throws -> FfiTrashEntry {
        try record(.deleteAsset(assetId, retainDays))
        let deletes = calls.count { if case .deleteAsset = $0 { true } else { false } }
        if let deleteFailsAt, deletes > deleteFailsAt { throw Failure() }
        return FfiTrashEntry(
            assetId: assetId,
            deletedAt: "2026-01-01T00:00:00Z",
            retentionUntil: "2026-01-31T00:00:00Z",
            restorable: true
        )
    }
}

private struct Failure: Error {}

/// Every batch the stream yields, in order, or the error it finished with.
private func collect(_ stream: AsyncThrowingStream<[CullAsset], any Error>) async throws -> [[CullAsset]] {
    var batches: [[CullAsset]] = []
    for try await batch in stream {
        batches.append(batch)
    }
    return batches
}

@Suite("FFICullPort — the workspace's cull verbs as the CullPort")
struct FFICullPortTests {
    private static func uuid(_ n: Int) -> String {
        String(format: "0192f0c4-0000-7000-8000-%012d", n)
    }

    private static let stack = "0192f0c4-9999-7000-8000-000000000001"

    @Test("the pass is every flag set in id order, with stacked assets placed and the rest lone")
    func readsThePass() async throws {
        let workspace = RecordingWorkspace()
        workspace.flags = [Self.uuid(3): .reject, Self.uuid(1): .pick, Self.uuid(2): .neutral, Self.uuid(4): .neutral]
        workspace.members = [
            FfiStackMember(assetId: Self.uuid(2), stackId: Self.stack, isCover: false, memberIndex: 1),
            FfiStackMember(assetId: Self.uuid(3), stackId: Self.stack, isCover: true, memberIndex: nil),
        ]
        let port = FFICullPort(source: workspace)

        let batches = try await collect(port.pass(over: TimelineQuery(includeStackHidden: true), batchSize: 200))

        let stackID = StackID(Self.stack)
        #expect(batches == [[
            CullAsset(id: .managed(uuid: Self.uuid(1)), cull: .pick),
            CullAsset(
                id: .managed(uuid: Self.uuid(2)),
                cull: .neutral,
                stack: CullStackPlace(stackID: stackID, isCover: false, memberIndex: 1)
            ),
            CullAsset(
                id: .managed(uuid: Self.uuid(3)),
                cull: .reject,
                stack: CullStackPlace(stackID: stackID, isCover: true)
            ),
            CullAsset(id: .managed(uuid: Self.uuid(4)), cull: .neutral),
        ]])
        #expect(workspace.calls == [
            .stackMemberships, .assetsByCull(.pick), .assetsByCull(.neutral), .assetsByCull(.reject),
        ])
    }

    @Test("the pass arrives in batches of at most the batch size, and an empty one yields none")
    func batchesThePass() async throws {
        let workspace = RecordingWorkspace()
        workspace.flags = Dictionary(uniqueKeysWithValues: (1 ... 5).map { (Self.uuid($0), FfiCullFlag.neutral) })
        let port = FFICullPort(source: workspace)

        let batches = try await collect(port.pass(over: .default, batchSize: 2))
        #expect(batches.map(\.count) == [2, 2, 1])
        #expect(batches.flatMap { $0 }.map(\.id) == (1 ... 5).map { .managed(uuid: Self.uuid($0)) })

        let empty = try await collect(FFICullPort(source: RecordingWorkspace()).pass(over: .default, batchSize: 2))
        #expect(empty.isEmpty)
    }

    @Test("a pass filtered to one flag reads only that flag set")
    func readsOneFlag() async throws {
        let workspace = RecordingWorkspace()
        workspace.flags = [Self.uuid(1): .pick, Self.uuid(2): .reject]
        let port = FFICullPort(source: workspace)

        let batches = try await collect(port.pass(over: TimelineQuery(cull: .reject), batchSize: 10))

        #expect(batches == [[CullAsset(id: .managed(uuid: Self.uuid(2)), cull: .reject)]])
        #expect(workspace.calls == [.stackMemberships, .assetsByCull(.reject)])
    }

    @Test(
        "a sequence the workspace cannot select is refused without a call across the boundary",
        arguments: [
            TimelineQuery(albumID: .managed(uuid: "0192f0c4-0000-7000-8000-0000000000aa")),
            TimelineQuery(slice: .trash),
            TimelineQuery(mediaKind: .video),
            TimelineQuery(capturedAfter: CapsuleTimestamp(epochSeconds: 0)),
            TimelineQuery(capturedBefore: CapsuleTimestamp(epochSeconds: 0)),
            TimelineQuery(minimumRating: 3),
            TimelineQuery(cull: .unknown("maybe")),
        ]
    )
    func refusesUnsupportedSequences(query: TimelineQuery) async {
        let workspace = RecordingWorkspace()
        let port = FFICullPort(source: workspace)

        await #expect(throws: CullPortRefusal.unsupportedSequence) {
            _ = try await collect(port.pass(over: query, batchSize: 10))
        }
        #expect(workspace.calls.isEmpty)
    }

    @Test("a failed read finishes the stream with the workspace's error")
    func readFailureFinishesTheStream() async {
        let workspace = RecordingWorkspace()
        workspace.failure = Failure()
        let port = FFICullPort(source: workspace)

        await #expect(throws: Failure.self) { _ = try await collect(port.pass(over: .default, batchSize: 10)) }
    }

    @Test("each asset is flagged by its UUID, with the workspace's flag", arguments: [
        (CullFlag.pick, FfiCullFlag.pick), (.neutral, .neutral), (.reject, .reject),
    ])
    func flagsAssets(flag: CullFlag, expected: FfiCullFlag) async throws {
        let workspace = RecordingWorkspace()
        let port = FFICullPort(source: workspace)

        try await port.setCull(flag, for: [.managed(uuid: Self.uuid(1)), .managed(uuid: Self.uuid(2))])

        #expect(workspace.calls == [.setCull(Self.uuid(1), expected), .setCull(Self.uuid(2), expected)])
    }

    @Test("a stack is flagged in one call, answering the members it reached")
    func flagsAStack() async throws {
        let workspace = RecordingWorkspace()
        workspace.stackAnswer = [Self.uuid(1), Self.uuid(2)]
        let port = FFICullPort(source: workspace)

        let flagged = try await port.flagStack(StackID(Self.stack), as: .reject)

        #expect(flagged == [.managed(uuid: Self.uuid(1)), .managed(uuid: Self.uuid(2))])
        #expect(workspace.calls == [.flagStack(Self.stack, .reject)])
    }

    @Test("an unknown flag and a system-Photos asset write nothing")
    func refusesWhatTheWorkspaceCannotHold() async {
        let workspace = RecordingWorkspace()
        let port = FFICullPort(source: workspace)
        let system = AssetID.photoKit(localIdentifier: "ph-1")

        await #expect(throws: CullPortRefusal.unknownFlag) {
            try await port.setCull(.unknown("maybe"), for: [.managed(uuid: Self.uuid(1))])
        }
        await #expect(throws: CullPortRefusal.unknownFlag) {
            _ = try await port.flagStack(StackID(Self.stack), as: .unknown("maybe"))
        }
        await #expect(throws: CullPortRefusal.notFound) {
            try await port.setCull(.pick, for: [.managed(uuid: Self.uuid(1)), system])
        }
        await #expect(throws: CullPortRefusal.notFound) {
            try await port.moveToTrash([.managed(uuid: Self.uuid(1)), system])
        }
        #expect(workspace.calls.isEmpty)
    }

    @Test("the sweep deletes exactly the given assets, each with the retention window")
    func sweepsTheGivenAssets() async throws {
        let workspace = RecordingWorkspace()
        let port = FFICullPort(source: workspace, retainDays: 7)

        try await port.moveToTrash([.managed(uuid: Self.uuid(1)), .managed(uuid: Self.uuid(2))])

        #expect(workspace.calls == [.deleteAsset(Self.uuid(1), 7), .deleteAsset(Self.uuid(2), 7)])
        #expect(FFICullPort.defaultRetainDays == 30)
    }

    @Test("a sweep that fails partway stops there and reports the failure")
    func sweepStopsAtTheFirstFailure() async {
        let workspace = RecordingWorkspace()
        workspace.deleteFailsAt = 1
        let port = FFICullPort(source: workspace)
        let ids = (1 ... 3).map { AssetID.managed(uuid: Self.uuid($0)) }

        await #expect(throws: Failure.self) { try await port.moveToTrash(ids) }
        #expect(workspace.calls == [.deleteAsset(Self.uuid(1), 30), .deleteAsset(Self.uuid(2), 30)])
    }
}
