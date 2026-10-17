import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import Synchronization

// MARK: - LibraryPageCache

/// The bounded, page-at-a-time row store behind ``PagedLibrarySnapshot``.
///
/// It exists because the two sides of the bridge disagree about shape:
/// ``AssetSnapshot`` promises **synchronous random access**, while
/// ``LibraryPort`` only answers **asynchronous windows**. Something has to hold
/// the windows that have arrived and answer synchronously from them, and that
/// something must be `Sendable` without an actor — an actor cannot serve a
/// synchronous `asset(at:)` at all.
///
/// Hence a `Mutex` rather than an actor. The critical sections are a dictionary
/// lookup and a short array splice; nothing awaits while holding the lock, so
/// there is no path where a scroll blocks on a fetch.
///
/// Two bounds keep it honest under a 250 000-asset library:
///
/// - **Residency.** Only ``residentPageLimit`` pages are kept; the
///   least-recently-touched is dropped. A long scroll therefore costs a fixed
///   amount of memory rather than materialising the library it scrolled past.
/// - **Concurrency.** At most ``concurrentFetchLimit`` fetches are in flight.
///   A consumer that walks `0 ..< count` synchronously would otherwise queue
///   one task per page — over a thousand of them on the huge-library scenario —
///   and spend the scroll servicing its own stampede.
final class LibraryPageCache: Sendable {
    /// Rows per window. Matches ``PageRequest/defaultLimit`` so the port is
    /// asked for the window size it was designed around.
    let pageSize: Int
    /// How many windows stay resident before the least-recently-used is dropped.
    let residentPageLimit: Int
    /// How many fetches may be in flight at once.
    let concurrentFetchLimit: Int

    private let library: any LibraryPort
    private let query: TimelineQuery
    private let state = Mutex(State())

    init(
        library: any LibraryPort,
        query: TimelineQuery,
        pageSize: Int = PageRequest.defaultLimit,
        residentPageLimit: Int = 40,
        concurrentFetchLimit: Int = 4
    ) {
        self.library = library
        self.query = query
        self.pageSize = max(1, pageSize)
        self.residentPageLimit = max(1, residentPageLimit)
        self.concurrentFetchLimit = max(1, concurrentFetchLimit)
    }

    /// The window an index falls in.
    func pageIndex(containing index: Int) -> Int {
        index / pageSize
    }

    /// The row at `index`, or `nil` when its window is not resident.
    ///
    /// Touching the LRU on a **hit** is what makes the residency bound follow
    /// the scroll rather than the fetch order.
    func asset(at index: Int) -> Asset? {
        let page = pageIndex(containing: index)
        let offset = index - page * pageSize
        return state.withLock { state in
            guard let rows = state.pages[page], offset < rows.count else { return nil }
            Self.touch(&state, page: page)
            return rows[offset]
        }
    }

    /// Ask for the window containing `index`, if there is room to ask.
    ///
    /// Deliberately fire-and-forget and deliberately *droppable*: when the
    /// in-flight budget is spent the request is discarded rather than queued,
    /// because a queued request from a scroll that has already moved on is
    /// worse than no request at all — the next read of the same index will ask
    /// again, by which time the budget has usually freed up.
    func scheduleLoad(containing index: Int) {
        let page = pageIndex(containing: index)
        let admitted = state.withLock { state -> Bool in
            guard state.pages[page] == nil,
                  !state.inFlight.contains(page),
                  state.inFlight.count < concurrentFetchLimit
            else { return false }
            state.inFlight.insert(page)
            return true
        }
        guard admitted else { return }
        Task { await load(page: page, alreadyReserved: true) }
    }

    /// Fetch a window and make it resident.
    func load(page: Int, alreadyReserved: Bool = false) async {
        if !alreadyReserved {
            let admitted = state.withLock { state -> Bool in
                guard state.pages[page] == nil, !state.inFlight.contains(page) else { return false }
                state.inFlight.insert(page)
                return true
            }
            guard admitted else { return }
        }
        let rows = await fetch(page: page)
        state.withLock { state in
            state.inFlight.remove(page)
            state.pages[page] = rows
            Self.touch(&state, page: page)
            Self.evict(&state, limit: residentPageLimit)
        }
    }

    /// Fetch the first `pageCount` windows in order, before anything is drawn.
    ///
    /// Sequential rather than concurrent: the point is that the first screens
    /// are real rows on the very first `asset(at:)`, and issuing them in order
    /// means the top of the timeline lands first.
    func warm(pageCount: Int) async {
        for page in 0 ..< max(0, pageCount) {
            await load(page: page)
        }
    }

    /// Windows currently resident — a residency assertion for tests.
    var loadedPageCount: Int {
        state.withLock { $0.pages.count }
    }

    /// Whether the window containing `index` has arrived.
    func isLoaded(containing index: Int) -> Bool {
        let page = pageIndex(containing: index)
        return state.withLock { $0.pages[page] != nil }
    }

    // MARK: Private

    /// One window from the port, projected onto ``Asset``.
    ///
    /// A failed read yields an empty window rather than propagating: this is a
    /// **local gallery read**, whose contract is that it never attempts the
    /// network and therefore has no transient failure worth surfacing mid-scroll.
    /// An empty window simply reads as "not here", and the next request retries.
    private func fetch(page: Int) async -> [Asset] {
        do {
            let window = try await library.assets(
                matching: query,
                offset: page * pageSize,
                limit: pageSize
            )
            return window.items.map(Asset.init(libraryAsset:))
        } catch {
            CapsuleLog.assetKit.error(
                "library page \(page, privacy: .public) failed: \(String(describing: error), privacy: .public)"
            )
            return []
        }
    }

    private static func touch(_ state: inout State, page: Int) {
        state.recency.removeAll { $0 == page }
        state.recency.append(page)
    }

    private static func evict(_ state: inout State, limit: Int) {
        while state.recency.count > limit {
            let dropped = state.recency.removeFirst()
            state.pages[dropped] = nil
        }
    }

    private struct State {
        var pages: [Int: [Asset]] = [:]
        /// Least-recently-touched first.
        var recency: [Int] = []
        var inFlight: Set<Int> = []
    }
}
