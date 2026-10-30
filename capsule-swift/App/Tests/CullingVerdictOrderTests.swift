import CapsuleDomain
import Foundation
import Testing

@testable import Capsule

/// Verdict writes reach the library in the order the verdicts were given, even
/// where an earlier verdict's write has to wait on the stack port first.
@Suite("Culling verdict write order")
@MainActor
struct CullingVerdictOrderTests {
    @Test("a stack verdict delayed on its members still lands before a later single-asset verdict")
    func delayedStackMembersKeepVerdictOrder() async throws {
        let library = Library()
        let gate = Gate()
        let queue = CullWriteQueue()
        let flagStack: CullVerdictWrite.FlagStack = { _, flag in
            await gate.wait()
            let members = [Fixture.id(1), Fixture.id(2)]
            try await library.setCull(flag, members)
            return members
        }
        let stackVerdict = CullVerdict(flag: .reject, stackID: Fixture.stackID, targets: [Fixture.id(1)], serial: 0)
        let assetVerdict = CullVerdict(flag: .pick, stackID: nil, targets: [Fixture.id(2)], serial: 1)

        for verdict in [stackVerdict, assetVerdict] {
            queue.submit {
                _ = try? await CullVerdictWrite.run(verdict, flagStack: flagStack, setCull: library.setCull)
            }
        }
        // Let the single-asset write run if anything would let it overtake.
        for _ in 0 ..< 20 {
            await Task.yield()
        }
        #expect(await library.writes.isEmpty)

        await gate.open()
        await queue.drain()

        #expect(await library.writes == [
            Write(flag: .reject, ids: [Fixture.id(1), Fixture.id(2)]),
            Write(flag: .pick, ids: [Fixture.id(2)]),
        ])
        #expect(await library.flags[Fixture.id(2)] == .pick)
        #expect(await library.flags[Fixture.id(1)] == .reject)
    }

    @Test("a failed write does not stop the writes queued behind it")
    func failureKeepsTheQueueRunning() async {
        let library = Library()
        let queue = CullWriteQueue()
        let flagStack: CullVerdictWrite.FlagStack = { _, _ in throw Failure() }
        let stackVerdict = CullVerdict(flag: .reject, stackID: Fixture.stackID, targets: [Fixture.id(1)], serial: 0)
        let assetVerdict = CullVerdict(flag: .pick, stackID: nil, targets: [Fixture.id(1)], serial: 1)
        let outcomes = Outcomes()

        for verdict in [stackVerdict, assetVerdict] {
            queue.submit {
                let written = try? await CullVerdictWrite.run(verdict, flagStack: flagStack, setCull: library.setCull)
                outcomes.values.append(written != nil)
            }
        }
        await queue.drain()

        #expect(outcomes.values == [false, true])
        #expect(await library.flags[Fixture.id(1)] == .pick)
    }

    @Test("a sweep submitted behind a delayed verdict write runs after it")
    func sweepWaitsForQueuedVerdicts() async {
        let gate = Gate()
        let queue = CullWriteQueue()
        let order = Outcomes()

        queue.submit {
            await gate.wait()
            order.values.append(true)
        }
        queue.submit { order.values.append(false) }
        for _ in 0 ..< 20 {
            await Task.yield()
        }
        #expect(order.values.isEmpty)

        await gate.open()
        await queue.drain()
        #expect(order.values == [true, false])
    }

    @Test("a queued sweep reads its candidates after the verdict write ahead of it settles")
    func sweepReadsCandidatesAfterQueuedWrites() async throws {
        let gate = Gate()
        let queue = CullWriteQueue()
        let rejected = Rejected()
        let trashed = Trashed()

        queue.submit {
            await gate.wait()
            rejected.ids = [Fixture.id(1)]
        }
        queue.submit {
            _ = try? await CullSweep.run(
                candidates: { rejected.ids },
                moveToTrash: { trashed.batches.append($0) }
            )
        }
        for _ in 0 ..< 20 {
            await Task.yield()
        }
        #expect(trashed.batches.isEmpty)

        await gate.open()
        await queue.drain()
        #expect(trashed.batches == [[Fixture.id(1)]])
    }

    @Test("a sweep with no candidates trashes nothing")
    func emptySweepIsANoOp() async throws {
        let trashed = Trashed()
        let result = try await CullSweep.run(candidates: { [] }, moveToTrash: { trashed.batches.append($0) })
        #expect(result == nil)
        #expect(trashed.batches.isEmpty)
    }

    @Test("a failed trash surfaces to the caller")
    func failedTrashThrows() async {
        await #expect(throws: Failure.self) {
            _ = try await CullSweep.run(candidates: { [Fixture.id(1)] }, moveToTrash: { _ in throw Failure() })
        }
    }

    @Test("a stack verdict flags the stack, then any target the stack no longer lists")
    func stackWriteTargets() async throws {
        let library = Library()
        let verdict = CullVerdict(
            flag: .pick,
            stackID: Fixture.stackID,
            targets: [Fixture.id(1), Fixture.id(3)],
            serial: 0
        )

        let written = try await CullVerdictWrite.run(
            verdict,
            flagStack: { stackID, flag in
                #expect(stackID == Fixture.stackID)
                try await library.setCull(flag, [Fixture.id(1), Fixture.id(2)])
                return [Fixture.id(1), Fixture.id(2)]
            },
            setCull: library.setCull
        )

        #expect(written == [Fixture.id(1), Fixture.id(2), Fixture.id(3)])
        #expect(await library.writes == [
            Write(flag: .pick, ids: [Fixture.id(1), Fixture.id(2)]),
            Write(flag: .pick, ids: [Fixture.id(3)]),
        ])
    }

    @Test("a stack verdict whose targets the stack all lists writes nothing more")
    func stackWriteCoveredByTheStack() async throws {
        let library = Library()
        let verdict = CullVerdict(flag: .reject, stackID: Fixture.stackID, targets: [Fixture.id(1)], serial: 0)

        let written = try await CullVerdictWrite.run(
            verdict,
            flagStack: { _, _ in [Fixture.id(1), Fixture.id(2)] },
            setCull: library.setCull
        )

        #expect(written == [Fixture.id(1), Fixture.id(2)])
        #expect(await library.writes.isEmpty)
    }

    @Test("a single-asset verdict never flags a stack")
    func singleAssetSkipsTheStackPort() async throws {
        let library = Library()
        let verdict = CullVerdict(flag: .neutral, stackID: nil, targets: [Fixture.id(4)], serial: 0)

        let written = try await CullVerdictWrite.run(
            verdict,
            flagStack: { _, _ in
                Issue.record("flagged a stack for a single-asset verdict")
                return []
            },
            setCull: library.setCull
        )

        #expect(written == [Fixture.id(4)])
        #expect(await library.writes == [Write(flag: .neutral, ids: [Fixture.id(4)])])
    }
}

// MARK: - Doubles

private struct Write: Equatable {
    let flag: CullFlag
    let ids: [AssetID]
}

private struct Failure: Error {}

/// Whether each queued write succeeded, in the order the writes ran.
@MainActor
private final class Outcomes {
    var values: [Bool] = []
}

@MainActor
private final class Rejected {
    var ids: [AssetID] = []
}

@MainActor
private final class Trashed {
    var batches: [[AssetID]] = []
}

/// The library's cull flags, recording each write in the order it arrived.
private actor Library {
    private(set) var writes: [Write] = []
    private(set) var flags: [AssetID: CullFlag] = [:]

    nonisolated var setCull: CullVerdictWrite.SetCull {
        { flag, ids in await self.record(flag, ids) }
    }

    private func record(_ flag: CullFlag, _ ids: [AssetID]) {
        writes.append(Write(flag: flag, ids: ids))
        for id in ids {
            flags[id] = flag
        }
    }
}

/// Holds the stack port's answer until the test opens it.
private actor Gate {
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        isOpen = true
        for waiter in waiters {
            waiter.resume()
        }
        waiters = []
    }
}

private enum Fixture {
    static let stackID = StackID("0192f0c0-9999-7000-8000-000000000009")

    static func id(_ number: Int) -> AssetID {
        .managed(uuid: String(format: "0192f0c0-0000-7000-8000-%012d", number))
    }
}
