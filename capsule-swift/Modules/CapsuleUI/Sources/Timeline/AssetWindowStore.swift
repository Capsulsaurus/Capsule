import Foundation
import Observation

// MARK: - AssetWindowStore

/// A sliding window of materialised rows over a collection too large to hold.
///
/// ``TimelineLayout`` answers *where* item 148 302 sits without knowing what it
/// is. This is the other half: it answers *what* item 148 302 is, for the few
/// thousand indices currently near the viewport, and forgets the rest.
///
/// The contract it exists to enforce is that **the app never materialises the
/// library**. A 250 000-asset timeline holds at most
/// ``Configuration/maximumResidentPages`` pages — a few thousand rows — no
/// matter how far it is scrolled, and scrolling from one end to the other
/// allocates no more than scrolling one screen.
///
/// Three behaviours make that work in practice rather than only in principle:
///
/// - **Fetches are cancelled, not awaited.** A fast scroll issues page requests
///   the user has already scrolled past. Awaiting them serialises the fetch the
///   user *is* waiting for behind a queue of fetches nobody wants, which is the
///   usual reason a virtualized grid feels worse than a naive one.
/// - **Stale results are discarded by generation.** ``invalidate()`` bumps a
///   counter; a fetch that returns against an old counter is dropped rather
///   than written, so a change that lands mid-scroll cannot resurrect rows from
///   the previous state of the library.
/// - **Required pages are never evicted.** LRU alone will happily drop the page
///   under the user's thumb when the window is large relative to the cap, which
///   produces a grid that flickers back to placeholders while standing still.
///
/// It is deliberately generic over a fetch closure rather than over a port:
/// `CapsuleUI` has no business knowing what a `LibraryPort` is, and a closure
/// makes the paging behaviour testable against a counter instead of a library.
@MainActor
@Observable
public final class AssetWindowStore<Element: Sendable & Equatable> {
    // MARK: Configuration

    /// The knobs that decide how much is resident and how far ahead it reads.
    public struct Configuration: Sendable, Equatable {
        /// Rows per fetch. Large enough that a screen is one or two round
        /// trips, small enough that a cancelled fetch wastes little.
        public var pageSize: Int
        /// How many pages may be resident at once. The memory ceiling.
        public var maximumResidentPages: Int
        /// How far beyond the viewport to keep loaded, in screenfuls. The
        /// budget for scrolling faster than the fetch can answer.
        public var marginScreens: Double

        public init(pageSize: Int = 200, maximumResidentPages: Int = 15, marginScreens: Double = 1.5) {
            self.pageSize = max(1, pageSize)
            self.maximumResidentPages = max(1, maximumResidentPages)
            self.marginScreens = max(0, marginScreens)
        }
    }

    /// Fetches `limit` rows starting at `offset`, in timeline order.
    ///
    /// A short return is the end of the collection, matching `Page.hasMore`.
    public typealias Fetch = @Sendable (_ offset: Int, _ limit: Int) async throws -> [Element]

    // MARK: Observable state

    /// How many rows the collection holds, from the same aggregate that sized
    /// the layout. Known before anything is fetched.
    public private(set) var totalCount: Int

    /// The most recent fetch failure, for a view that wants to surface one.
    ///
    /// Not cleared by a successful fetch of a *different* page: a store with
    /// one unreachable page is still a store with an error worth showing.
    /// ``clearError()`` is explicit.
    public private(set) var lastError: (any Error)?

    /// Bumped whenever resident content changes, so an `@Observable` reader
    /// re-renders on a page arriving.
    ///
    /// Observation tracks property *access*, and a view that reads rows through
    /// ``element(at:)`` for indices that are not yet loaded reads no stored
    /// property at all — so without this it would never be re-invoked when they
    /// arrive. Reading it in a view body is the subscription.
    public private(set) var revision: Int = 0

    // MARK: Private state

    // Everything below is `@ObservationIgnored` on purpose. `@Observable` tracks
    // every stored property by default, and `element(at:)` — which a view body
    // calls for each visible cell — updates LRU bookkeeping as a side effect.
    // Tracked, that would be a mutation during view update and an endless
    // invalidation loop. Residency is instead published through ``revision``,
    // which changes only when content actually arrives or leaves.
    @ObservationIgnored private let fetch: Fetch
    @ObservationIgnored private var configuration: Configuration

    /// Resident pages by page index. A page is a contiguous `pageSize` run.
    @ObservationIgnored private var pages: [Int: [Element]] = [:]
    /// Monotonic use-order for LRU. A counter rather than a clock so eviction
    /// is deterministic and testable without injecting time.
    @ObservationIgnored private var lastTouched: [Int: Int] = [:]
    @ObservationIgnored private var tick = 0

    @ObservationIgnored private var inFlight: [Int: Task<Void, Never>] = [:]
    /// Pages whose fetch returned fewer rows than asked for, or which lie past
    /// the end. Re-requesting them is pure waste.
    @ObservationIgnored private var exhausted: Set<Int> = []

    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var requiredPages: Range<Int> = 0 ..< 0

    // MARK: Init

    /// - Note: there is deliberately no `deinit` cancelling outstanding fetches.
    ///   A `@MainActor` type's `deinit` is not actor-isolated and cannot reach
    ///   isolated storage. Outstanding tasks hold `self` weakly, so they resolve
    ///   into nothing; a screen that wants them stopped sooner calls
    ///   ``cancelOutstandingFetches()`` from its own teardown.
    public init(totalCount: Int = 0, configuration: Configuration = Configuration(), fetch: @escaping Fetch) {
        self.totalCount = max(0, totalCount)
        self.configuration = configuration
        self.fetch = fetch
    }

    // MARK: Reading

    /// The row at `index`, or `nil` when it is not resident yet.
    ///
    /// `nil` is a normal, frequent answer and **not** an error: the caller
    /// renders the asset's dominant colour or its LQIP and gets the real row on
    /// a later frame. A store that instead blocked or faulted here would make
    /// every fast scroll a stutter.
    public func element(at index: Int) -> Element? {
        _ = revision
        guard index >= 0, index < totalCount else { return nil }
        let page = index / configuration.pageSize
        guard let rows = pages[page] else { return nil }
        let withinPage = index % configuration.pageSize
        guard withinPage < rows.count else { return nil }
        touch(page)
        return rows[withinPage]
    }

    public subscript(index: Int) -> Element? { element(at: index) }

    /// Whether `index` can be rendered without waiting.
    public func isLoaded(at index: Int) -> Bool { element(at: index) != nil }

    /// Resident page count — the assertion target for the memory ceiling.
    public var residentPageCount: Int {
        _ = revision
        return pages.count
    }

    /// Resident row count, for tests and for the debug overlay.
    public var residentRowCount: Int {
        _ = revision
        return pages.values.reduce(0) { $0 + $1.count }
    }

    /// Whether any fetch is outstanding, for a loading affordance.
    public var isLoading: Bool {
        _ = revision
        return !inFlight.isEmpty
    }

    public func clearError() { lastError = nil }

    /// Refetch the pages whose last fetch failed and that the viewport still needs.
    ///
    /// The store deliberately does not do this on its own. ``setVisibleRange(_:viewportItemCount:)``
    /// no-ops on an unchanged range, so a viewport sitting still over a failed page
    /// holds its placeholder until the user scrolls away and back — and the obvious
    /// repair, refilling on every call, would re-issue a failing fetch every frame
    /// for as long as the user looked at it. Neither is acceptable, so the retry is
    /// explicit and belongs to whatever surfaces ``lastError``.
    ///
    /// Clears ``lastError`` first: a retry the user asked for should not leave the
    /// previous failure on screen while it is in flight.
    public func retryFailedPages() {
        lastError = nil
        for page in requiredPages where pages[page] == nil && inFlight[page] == nil && !exhausted.contains(page) {
            startFetch(page)
        }
        bumpRevision()
    }

    /// Stop every outstanding fetch — for a screen tearing down while a slow
    /// page is still in flight.
    public func cancelOutstandingFetches() {
        for task in inFlight.values {
            task.cancel()
        }
        inFlight.removeAll()
        bumpRevision()
    }

    // MARK: Driving the window

    /// Tell the store what is on screen.
    ///
    /// `viewportItemCount` is how many rows one screenful holds — the unit
    /// ``Configuration/marginScreens`` is denominated in. Passing the visible
    /// range's own count is a reasonable default, but a grid that knows its
    /// column count can be exact.
    ///
    /// Safe to call every frame: identical input is a no-op, so a scroll that
    /// stays within one page issues nothing.
    ///
    /// That no-op is total — it covers a page whose fetch *failed* as much as one
    /// that succeeded. Recovering from a failure without moving the viewport is
    /// ``retryFailedPages()``, and that is deliberate: retrying from here would put
    /// a failing fetch on every frame.
    public func setVisibleRange(_ visible: Range<Int>, viewportItemCount: Int? = nil) {
        guard totalCount > 0 else { return }

        let clamped = visible.clamped(to: 0 ..< totalCount)
        let screenful = max(1, viewportItemCount ?? clamped.count)
        let margin = Int((Double(screenful) * configuration.marginScreens).rounded())

        let lower = max(0, clamped.lowerBound - margin)
        let upper = min(totalCount, clamped.upperBound + margin)
        guard lower < upper else { return }

        let firstPage = lower / configuration.pageSize
        let lastPage = (upper - 1) / configuration.pageSize
        let required = firstPage ..< (lastPage + 1)
        guard required != requiredPages else { return }
        requiredPages = required

        cancelFetchesOutside(required)
        for page in required where pages[page] == nil && inFlight[page] == nil && !exhausted.contains(page) {
            startFetch(page)
        }
        evictIfNeeded()
    }

    /// Replace the collection's size — a new query, or a change in what matches.
    ///
    /// Drops every resident page: rows are addressed by *offset*, so a size
    /// change means index 400 is no longer the same asset, and keeping the old
    /// rows would show the right count of the wrong photos.
    public func reset(totalCount: Int) {
        self.totalCount = max(0, totalCount)
        invalidate()
    }

    /// Discard everything resident and re-fetch what is required.
    ///
    /// The response to a `LibraryChange`. Bumping the generation first is what
    /// makes it safe mid-scroll: fetches already in flight will return against
    /// the old generation and be dropped rather than written.
    public func invalidate() {
        generation &+= 1
        for task in inFlight.values {
            task.cancel()
        }
        inFlight.removeAll()
        pages.removeAll()
        lastTouched.removeAll()
        exhausted.removeAll()
        let required = requiredPages
        requiredPages = 0 ..< 0
        bumpRevision()
        guard !required.isEmpty else { return }
        requiredPages = required
        for page in required {
            startFetch(page)
        }
    }

    /// Change paging behaviour at runtime — a density change alters how many
    /// rows a screenful holds, which changes the useful margin.
    public func apply(_ configuration: Configuration) {
        guard configuration != self.configuration else { return }
        let sizeChanged = configuration.pageSize != self.configuration.pageSize
        self.configuration = configuration
        // Page indices are derived from `pageSize`, so resident pages are only
        // meaningful while it holds. Anything else can keep them.
        if sizeChanged { invalidate() } else { evictIfNeeded() }
    }

    // MARK: Fetching

    private func startFetch(_ page: Int) {
        let offset = page * configuration.pageSize
        guard offset < totalCount else {
            exhausted.insert(page)
            return
        }
        let limit = min(configuration.pageSize, totalCount - offset)
        let requestGeneration = generation
        let fetch = fetch

        inFlight[page] = Task { [weak self] in
            let result: Result<[Element], any Error>
            do {
                result = try await .success(fetch(offset, limit))
            } catch {
                result = .failure(error)
            }
            guard !Task.isCancelled else { return }
            self?.receive(result, page: page, requested: limit, generation: requestGeneration)
        }
        // `isLoading` is derived from `inFlight`, which is untracked, so a
        // loading affordance would never appear without this.
        bumpRevision()
    }

    private func receive(
        _ result: Result<[Element], any Error>,
        page: Int,
        requested: Int,
        generation requestGeneration: Int
    ) {
        inFlight[page] = nil
        // The library moved under this fetch. Its rows describe a collection
        // that no longer exists.
        guard requestGeneration == generation else { return }

        switch result {
        case let .success(rows):
            if rows.isEmpty {
                exhausted.insert(page)
            } else {
                pages[page] = rows
                touch(page)
                if rows.count < requested { exhausted.insert(page) }
            }
            evictIfNeeded()
        case let .failure(error):
            // Not marked exhausted: a failure is refetchable, whereas a short read
            // is not. Refetching happens when the viewport next *moves* over the
            // page, or on an explicit ``retryFailedPages()`` — never on its own from
            // a stationary viewport, which would mean a fetch per frame.
            if !(error is CancellationError) { lastError = error }
        }
        bumpRevision()
    }

    private func cancelFetchesOutside(_ required: Range<Int>) {
        for (page, task) in inFlight where !required.contains(page) {
            task.cancel()
            inFlight[page] = nil
        }
    }

    // MARK: Residency

    private func touch(_ page: Int) {
        tick &+= 1
        lastTouched[page] = tick
    }

    /// Evict least-recently-used pages down to the cap, never touching a page
    /// the viewport currently needs.
    ///
    /// The pin matters more than the LRU order does. With a wide margin and a
    /// small cap, pure LRU evicts pages that are about to be read again on the
    /// very next frame, and the grid oscillates between rows and placeholders
    /// while the user is not even scrolling.
    private func evictIfNeeded() {
        guard pages.count > configuration.maximumResidentPages else { return }
        let evictable = pages.keys
            .filter { !requiredPages.contains($0) }
            .sorted { (lastTouched[$0] ?? 0) < (lastTouched[$1] ?? 0) }

        var overflow = pages.count - configuration.maximumResidentPages
        for page in evictable where overflow > 0 {
            pages[page] = nil
            lastTouched[page] = nil
            overflow -= 1
        }
    }

    private func bumpRevision() { revision &+= 1 }
}
