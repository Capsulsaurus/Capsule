import CapsuleDomain
import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleMock

// MARK: - Paging

/// The aggregate and the page must describe the same result set.
///
/// A virtualized grid sizes its sections from `dayCounts(...)` and then fetches
/// rows by offset. If those two disagree by even one asset, the grid draws the
/// wrong photo under the wrong header — and does so silently, several thousand
/// rows into a scroll.
@Suite("Aggregates agree with paged reads")
struct MockPagingTests {
    private func makeStore(assetCount: Int = 4000) -> MockLibraryStore {
        var configuration = MockConfiguration.make(scenario: .healthy)
        configuration.profile.assetCount = assetCount
        return MockLibraryStore(configuration: configuration)
    }

    /// Page through everything a query selects, tallying by day.
    private func tallyByDay(
        _ store: MockLibraryStore,
        query: TimelineQuery,
        pageSize: Int = 500
    ) async throws -> [DayKey: Int] {
        var tally: [DayKey: Int] = [:]
        var offset = 0
        while true {
            let page = try await store.assets(matching: query, offset: offset, limit: pageSize)
            for asset in page.items {
                tally[asset.dayKey, default: 0] += 1
            }
            offset += page.items.count
            if page.items.count < pageSize { break }
        }
        return tally
    }

    @Test("Unfiltered day counts equal what paging returns")
    func unfilteredCountsAgree() async throws {
        let store = makeStore()
        let counts = try await store.dayCounts(matching: .default)
        let tally = try await tallyByDay(store, query: .default)
        #expect(tally.count == counts.count)
        for count in counts {
            #expect(tally[count.dayKey] == count.count)
        }
        #expect(try await store.assetCount(matching: .default) == counts.totalCount)
    }

    /// The filtered path is a different code path — a full facet scan rather
    /// than the day boundary array — so it has to be checked separately or the
    /// fast path's correctness proves nothing about it.
    @Test(
        "Filtered day counts equal what paging returns",
        arguments: [
            TimelineQuery(mediaKind: .video),
            TimelineQuery(minimumRating: 4),
            TimelineQuery(cull: .reject),
            TimelineQuery(mediaKind: .image, minimumRating: 1),
        ]
    )
    func filteredCountsAgree(query: TimelineQuery) async throws {
        let store = makeStore()
        let counts = try await store.dayCounts(matching: query)
        let tally = try await tallyByDay(store, query: query)
        #expect(tally.count == counts.count)
        for count in counts {
            #expect(tally[count.dayKey] == count.count)
        }
        #expect(try await store.assetCount(matching: query) == counts.totalCount)
    }

    /// Album membership is the one facet a mutation can move, so it gets its own
    /// case: the aggregate has to follow the write.
    @Test("Album-filtered counts agree and follow a move")
    func albumCountsFollowAMove() async throws {
        let store = makeStore()
        let albums = try await store.containerAlbums()
        let target = albums[2].id
        let before = try await store.assetCount(matching: TimelineQuery(albumID: target))
        let moved = try await store.assets(matching: .default, offset: 0, limit: 3).items
            .filter { $0.albumID != target }
        try await store.move(moved.map(\.id), to: target)
        let after = try await store.assetCount(matching: TimelineQuery(albumID: target))
        #expect(after == before + moved.count)
        let counts = try await store.dayCounts(matching: TimelineQuery(albumID: target))
        #expect(counts.totalCount == after)
    }

    @Test("A page past the end is an empty final page, not a crash")
    func offsetPastTheEndIsEmpty() async throws {
        let store = makeStore(assetCount: 120)
        let page = try await store.assets(matching: .default, offset: 5000, limit: 50)
        #expect(page.items.isEmpty)
        #expect(page.hasMore == false)
        #expect(page.nextRequest == nil)
    }

    @Test("The last page is partial and reports no more")
    func lastPageIsPartial() async throws {
        let store = makeStore(assetCount: 120)
        let page = try await store.assets(matching: .default, offset: 100, limit: 50)
        #expect(page.items.count == 20)
        #expect(page.hasMore == false)
    }

    @Test("A full page in the middle reports more")
    func middlePageReportsMore() async throws {
        let store = makeStore(assetCount: 120)
        let page = try await store.assets(matching: .default, offset: 0, limit: 50)
        #expect(page.items.count == 50)
        #expect(page.hasMore)
        #expect(page.nextRequest?.offset == 50)
    }

    /// Windows must tile without gaps or repeats — the property a grid relies on
    /// when it prefetches ahead of the visible range.
    @Test("Consecutive windows tile the result exactly")
    func windowsTileExactly() async throws {
        let store = makeStore(assetCount: 500)
        var identifiers: [AssetID] = []
        for offset in stride(from: 0, to: 500, by: 73) {
            let page = try await store.assets(matching: .default, offset: offset, limit: 73)
            identifiers.append(contentsOf: page.items.map(\.id))
        }
        #expect(identifiers.count == 500)
        #expect(Set(identifiers).count == 500)
    }

    /// Expanding a stack adds members without disturbing the ordering the
    /// aggregate promised.
    @Test("Expanded stacks stay in order and stay counted")
    func expandedStacksAgree() async throws {
        let store = makeStore(assetCount: 800)
        let query = TimelineQuery(includeStackHidden: true)
        let counts = try await store.dayCounts(matching: query)
        let tally = try await tallyByDay(store, query: query)
        for count in counts {
            #expect(tally[count.dayKey] == count.count)
        }
        #expect(try await counts.totalCount > (store.assetCount(matching: .default)))
        let page = try await store.assets(matching: query, offset: 0, limit: 200)
        for (earlier, later) in zip(page.items, page.items.dropFirst()) {
            #expect(LibraryAsset.isOrderedNewestFirst(earlier, later))
        }
    }
}
