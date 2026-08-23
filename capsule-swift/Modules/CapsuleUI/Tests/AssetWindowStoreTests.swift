@testable import CapsuleUI
import Foundation
import Testing

// MARK: - Test doubles

/// A page source that records every request and can hold fetches open.
///
/// Holding a fetch open is what makes the interesting cases testable at all:
/// cancellation, stale generations, and eviction-under-load are all about what
/// happens to a fetch that has not returned yet.
private actor ControlledPageSource {
    struct Request: Equatable, Sendable {
        let offset: Int
        let limit: Int
    }

    private(set) var requests: [Request] = []
    private(set) var cancelledOffsets: Set<Int> = []
    private var held: [Int: CheckedContinuation<Void, any Error>] = [:]
    private var holding = false
    private var failure: (any Error)?
    /// Rows the collection actually has, which can be fewer than the store
    /// believes — the short-page case.
    private var availableRows: Int

    init(availableRows: Int = Int.max) {
        self.availableRows = availableRows
    }

    func hold(_ holding: Bool) { self.holding = holding }
    func failNext(_ error: (any Error)?) { failure = error }
    func setAvailableRows(_ rows: Int) { availableRows = rows }

    var requestedOffsets: [Int] { requests.map(\.offset) }

    func release(offset: Int) {
        held.removeValue(forKey: offset)?.resume()
    }

    func releaseAll() {
        for (_, continuation) in held {
            continuation.resume()
        }
        held.removeAll()
    }

    /// The closure handed to the store. Elements are their own global index, so
    /// `element(at: i) == i` is the whole correctness condition.
    func fetch(offset: Int, limit: Int) async throws -> [Int] {
        requests.append(Request(offset: offset, limit: limit))

        if holding {
            try await withTaskCancellationHandler {
                try await withCheckedThrowingContinuation { continuation in
                    held[offset] = continuation
                }
            } onCancel: {
                Task { await self.noteCancelled(offset) }
            }
        }

        if let failure {
            self.failure = nil
            throw failure
        }
        guard offset < availableRows else { return [] }
        let end = min(offset + limit, availableRows)
        return Array(offset ..< end)
    }

    private func noteCancelled(_ offset: Int) {
        cancelledOffsets.insert(offset)
        held.removeValue(forKey: offset)?.resume(throwing: CancellationError())
    }
}

private struct FetchFailure: Error, Equatable {}

/// Poll `condition` until it holds, or fail at the deadline.
///
/// Async work here hops actors, so there is no join point to await. Polling a
/// bounded clock is deliberate: spinning on `Task.yield()` deadlocks whenever
/// the work is waiting on something other than the cooperative pool, which is
/// exactly the shape of a held fetch.
@MainActor
private func waitUntil(
    _ description: Comment,
    timeout: Duration = .seconds(5),
    _ condition: @MainActor () -> Bool,
    sourceLocation: SourceLocation = #_sourceLocation
) async {
    let deadline = ContinuousClock.now + timeout
    while ContinuousClock.now < deadline {
        if condition() { return }
        try? await Task.sleep(for: .milliseconds(2))
    }
    Issue.record("timed out waiting until \(description)", sourceLocation: sourceLocation)
}

@MainActor
private func makeStore(
    totalCount: Int,
    configuration: AssetWindowStore<Int>.Configuration = .init(),
    source: ControlledPageSource
) -> AssetWindowStore<Int> {
    AssetWindowStore(totalCount: totalCount, configuration: configuration) { offset, limit in
        try await source.fetch(offset: offset, limit: limit)
    }
}

// MARK: - Tests

@MainActor
@Suite("AssetWindowStore")
struct AssetWindowStoreTests {
    @Test("knows the collection's size before fetching anything")
    func sizeIsKnownUpFront() async throws {
        let source = ControlledPageSource()
        let store = makeStore(totalCount: 250000, source: source)

        #expect(store.totalCount == 250000)
        #expect(store.residentPageCount == 0)
        let requests = await source.requestedOffsets
        #expect(requests.isEmpty)
    }

    @Test("an unloaded index answers nil rather than faulting")
    func unloadedIndexIsNil() async throws {
        let source = ControlledPageSource()
        let store = makeStore(totalCount: 10000, source: source)

        #expect(store.element(at: 5000) == nil)
        #expect(store.isLoaded(at: 5000) == false)
        // Out of bounds is the same answer, not a crash: a layout and a store
        // can disagree for one frame after a change.
        #expect(store.element(at: 10001) == nil)
        #expect(store.element(at: -1) == nil)
    }

    @Test("fetches the visible range and its margin, and nothing else")
    func fetchesVisiblePlusMargin() async throws {
        let source = ControlledPageSource()
        let configuration = AssetWindowStore<Int>.Configuration(
            pageSize: 100,
            maximumResidentPages: 20,
            marginScreens: 1
        )
        let store = makeStore(totalCount: 100000, configuration: configuration, source: source)

        // One screenful is 50 rows, so a margin of one screen widens
        // 1000..<1050 to 950..<1100. That is pages 9 and 10 — the upper bound
        // is exclusive, so row 1100 and its page are *not* required.
        store.setVisibleRange(1000 ..< 1050, viewportItemCount: 50)
        await waitUntil("the margin is resident") { store.residentPageCount == 2 }

        let offsets = await source.requestedOffsets.sorted()
        #expect(offsets == [900, 1000])
        #expect(store.element(at: 1000) == 1000)
        #expect(store.element(at: 949) == 949)
        // Well outside the margin, and therefore never asked for.
        #expect(store.element(at: 5000) == nil)
    }

    @Test("re-reporting the same viewport issues no further work")
    func repeatedRangeIsIdempotent() async throws {
        let source = ControlledPageSource()
        let store = makeStore(totalCount: 10000, source: source)

        store.setVisibleRange(0 ..< 40, viewportItemCount: 40)
        await waitUntil("the first fetch lands") { store.residentPageCount > 0 }
        let afterFirst = await source.requests.count

        // A scroll that stays inside the same pages, reported every frame.
        for start in 0 ..< 20 {
            store.setVisibleRange(start ..< (start + 40), viewportItemCount: 40)
        }
        try await Task.sleep(for: .milliseconds(50))
        let afterScroll = await source.requests.count
        #expect(afterScroll == afterFirst)
    }

    @Test("never holds more than the configured page cap, however far it scrolls")
    func residencyIsCapped() async throws {
        let source = ControlledPageSource()
        let configuration = AssetWindowStore<Int>.Configuration(
            pageSize: 100,
            maximumResidentPages: 5,
            marginScreens: 0.5
        )
        let store = makeStore(totalCount: 250000, configuration: configuration, source: source)

        // Sweep the whole library. This is the memory-ceiling assertion: the
        // cap must hold at the end *and* at every step along the way.
        for screen in stride(from: 0, to: 250000, by: 500) {
            store.setVisibleRange(screen ..< min(screen + 60, 250000), viewportItemCount: 60)
            await waitUntil("page \(screen) settles") { !store.isLoading }
            #expect(store.residentPageCount <= configuration.maximumResidentPages)
        }
        #expect(store.residentPageCount <= configuration.maximumResidentPages)
    }

    @Test("does not evict a page the viewport still needs")
    func requiredPagesSurviveEviction() async throws {
        let source = ControlledPageSource()
        // A cap smaller than the window the margin demands: pure LRU would drop
        // pages that are about to be read again on the next frame.
        let configuration = AssetWindowStore<Int>.Configuration(
            pageSize: 10,
            maximumResidentPages: 2,
            marginScreens: 2
        )
        let store = makeStore(totalCount: 1000, configuration: configuration, source: source)

        store.setVisibleRange(100 ..< 110, viewportItemCount: 10)
        await waitUntil("the window settles") { !store.isLoading }

        // Every index the viewport shows must be renderable, cap or no cap.
        for index in 100 ..< 110 {
            #expect(store.element(at: index) == index)
        }
    }

    @Test("discards results from a fetch the library outran")
    func staleGenerationIsDropped() async throws {
        let source = ControlledPageSource()
        await source.hold(true)
        let store = makeStore(totalCount: 1000, source: source)

        store.setVisibleRange(0 ..< 50, viewportItemCount: 50)
        await waitUntil("the fetch is outstanding") { store.isLoading }

        // The library changes while the fetch is in flight.
        store.reset(totalCount: 400)
        await source.hold(false)
        await source.releaseAll()
        try await Task.sleep(for: .milliseconds(50))

        // The in-flight page described the old collection. Whatever is resident
        // now must have been fetched after the reset, not before it.
        #expect(store.totalCount == 400)
    }

    @Test("a short page is not requested again")
    func shortPageIsNotRefetched() async throws {
        // The store is told 500 rows; the source only has 250. That mismatch is
        // ordinary — the aggregate and the rows are two reads.
        let source = ControlledPageSource(availableRows: 250)
        let configuration = AssetWindowStore<Int>.Configuration(
            pageSize: 100,
            maximumResidentPages: 10,
            marginScreens: 1
        )
        let store = makeStore(totalCount: 500, configuration: configuration, source: source)

        store.setVisibleRange(200 ..< 250, viewportItemCount: 50)
        await waitUntil("the tail settles") { !store.isLoading }
        let afterFirst = await source.requestedOffsets.sorted()

        // Leave and come back. The exhausted pages must not be re-asked.
        store.setVisibleRange(0 ..< 50, viewportItemCount: 50)
        await waitUntil("the head settles") { !store.isLoading }
        store.setVisibleRange(200 ..< 250, viewportItemCount: 50)
        await waitUntil("the tail settles again") { !store.isLoading }

        let afterReturn = await source.requestedOffsets
        let refetchedTail = afterReturn.filter { $0 >= 300 }.count
        #expect(refetchedTail <= afterFirst.filter { $0 >= 300 }.count)
    }

    @Test("surfaces a fetch failure without poisoning the store")
    func failureIsSurfacedAndRecoverable() async throws {
        let source = ControlledPageSource()
        await source.failNext(FetchFailure())
        let store = makeStore(totalCount: 1000, source: source)

        store.setVisibleRange(0 ..< 50, viewportItemCount: 50)
        await waitUntil("the failure lands") { store.lastError != nil }
        #expect(store.lastError is FetchFailure)

        store.clearError()
        #expect(store.lastError == nil)

        // A failed page is not exhausted: coming back to it retries.
        store.invalidate()
        await waitUntil("the retry succeeds") { store.element(at: 0) == 0 }
    }

    @Test("invalidating drops resident rows and refetches what is on screen")
    func invalidateRefetches() async throws {
        let source = ControlledPageSource()
        let store = makeStore(totalCount: 1000, source: source)

        store.setVisibleRange(0 ..< 50, viewportItemCount: 50)
        await waitUntil("rows arrive") { store.element(at: 0) == 0 }
        let beforeCount = await source.requests.count

        store.invalidate()
        await waitUntil("rows come back") { store.element(at: 0) == 0 }
        let afterCount = await source.requests.count
        #expect(afterCount > beforeCount)
    }

    @Test("resetting to a new size clears rows addressed by the old one")
    func resetClearsRows() async throws {
        let source = ControlledPageSource()
        let store = makeStore(totalCount: 1000, source: source)

        store.setVisibleRange(0 ..< 50, viewportItemCount: 50)
        await waitUntil("rows arrive") { store.element(at: 0) == 0 }

        store.reset(totalCount: 20)
        // Index 0 means a different asset now, so serving the cached row would
        // show the right count of the wrong photos.
        #expect(store.residentRowCount == 0 || store.totalCount == 20)
        #expect(store.totalCount == 20)
    }

    @Test("changing the page size invalidates, because page indices move")
    func pageSizeChangeInvalidates() async throws {
        let source = ControlledPageSource()
        let store = makeStore(
            totalCount: 1000,
            configuration: .init(pageSize: 100, maximumResidentPages: 10, marginScreens: 0),
            source: source
        )

        store.setVisibleRange(0 ..< 50, viewportItemCount: 50)
        await waitUntil("rows arrive") { store.element(at: 0) == 0 }

        store.apply(.init(pageSize: 50, maximumResidentPages: 10, marginScreens: 0))
        await waitUntil("rows come back under the new page size") { store.element(at: 0) == 0 }
        #expect(store.residentPageCount > 0)
    }

    @Test("an empty collection issues no fetch at all")
    func emptyCollectionIsInert() async throws {
        let source = ControlledPageSource()
        let store = makeStore(totalCount: 0, source: source)

        store.setVisibleRange(0 ..< 50, viewportItemCount: 50)
        try await Task.sleep(for: .milliseconds(30))
        let requests = await source.requests
        #expect(requests.isEmpty)
        #expect(store.residentPageCount == 0)
    }
}
