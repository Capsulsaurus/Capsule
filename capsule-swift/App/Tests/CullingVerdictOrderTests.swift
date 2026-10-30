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
        let members: CullVerdictWrite.Members = { _ in
            await gate.wait()
            return [Fixture.id(1), Fixture.id(2)]
        }
        let stackVerdict = CullVerdict(flag: .reject, stackID: Fixture.stackID, targets: [Fixture.id(1)], serial: 0)
        let assetVerdict = CullVerdict(flag: .pick, stackID: nil, targets: [Fixture.id(2)], serial: 1)

        for verdict in [stackVerdict, assetVerdict] {
            queue.submit {
                _ = try? await CullVerdictWrite.run(verdict, members: members, setCull: library.setCull)
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
        let members: CullVerdictWrite.Members = { _ in throw Failure() }
        let stackVerdict = CullVerdict(flag: .reject, stackID: Fixture.stackID, targets: [Fixture.id(1)], serial: 0)
        let assetVerdict = CullVerdict(flag: .pick, stackID: nil, targets: [Fixture.id(1)], serial: 1)
        let outcomes = Outcomes()

        for verdict in [stackVerdict, assetVerdict] {
            queue.submit {
                let written = try? await CullVerdictWrite.run(verdict, members: members, setCull: library.setCull)
                outcomes.values.append(written != nil)
            }
        }
        await queue.drain()

        #expect(outcomes.values == [false, true])
        #expect(await library.flags[Fixture.id(1)] == .pick)
    }

    @Test("a stack verdict writes every listed member, then any target the port missed")
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
            members: { _ in [Fixture.id(1), Fixture.id(2)] },
            setCull: library.setCull
        )

        #expect(written == [Fixture.id(1), Fixture.id(2), Fixture.id(3)])
        #expect(await library.writes == [Write(flag: .pick, ids: written)])
    }

    @Test("a single-asset verdict never asks the stack port")
    func singleAssetSkipsTheStackPort() async throws {
        let library = Library()
        let verdict = CullVerdict(flag: .neutral, stackID: nil, targets: [Fixture.id(4)], serial: 0)

        let written = try await CullVerdictWrite.run(
            verdict,
            members: { _ in
                Issue.record("asked the stack port for a single-asset verdict")
                return []
            },
            setCull: library.setCull
        )

        #expect(written == [Fixture.id(4)])
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
