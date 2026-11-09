import Foundation
import Testing

@testable import Capsule

/// Confirming the sweep queues it behind the verdict writes ahead of it.
@Suite("Culling sweep confirm routing")
@MainActor
struct CullingSweepConfirmRoutingTests {
    @Test("a confirmed sweep waits for a verdict write still in flight")
    func confirmedSweepWaitsForQueuedWrites() async {
        let queue = CullWriteQueue()
        let gate = Gate()
        let order = Order()

        queue.submit {
            await gate.wait()
            order.events.append("write")
        }
        CullSweep.confirm(on: queue) { order.events.append("sweep") }
        for _ in 0 ..< 20 {
            await Task.yield()
        }
        #expect(order.events.isEmpty)

        await gate.open()
        await queue.drain()
        #expect(order.events == ["write", "sweep"])
    }

    @Test("a confirmed sweep on an idle queue still runs")
    func confirmedSweepRunsOnIdleQueue() async {
        let queue = CullWriteQueue()
        let order = Order()

        CullSweep.confirm(on: queue) { order.events.append("sweep") }
        await queue.drain()

        #expect(order.events == ["sweep"])
    }
}

@MainActor
private final class Order {
    var events: [String] = []
}

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
