import Foundation
import Testing

import CapsuleDomain
import CapsuleFoundation
import CapsulePorts

// MARK: - Stub adapters

/// A minimal in-test ``LibraryPort``.
///
/// It exists to prove three things a protocol-only module can genuinely get
/// wrong, and that no amount of reading the declarations would catch:
///
/// 1. The ports are **conformable by an actor** — they are `Sendable` with
///    `async throws` members, so the real UniFFI adapter (which will own
///    mutable connection state) can implement them without redesign.
/// 2. The signatures are **usable**: paging composes, and the day-count
///    aggregate turns into the section offsets a grid needs.
/// 3. Swapping an implementation is a **one-line change** at the composition
///    root, because feature code depends only on `any LibraryPort`.
private actor StubLibrary: LibraryPort {
    private let assets: [LibraryAsset]
    private var continuation: AsyncStream<LibraryChange>.Continuation?

    init(assets: [LibraryAsset]) {
        self.assets = assets.sorted(by: LibraryAsset.isOrderedNewestFirst)
    }

    private func matching(_ query: TimelineQuery) -> [LibraryAsset] {
        assets.filter(query.admitsVisibility(of:))
    }

    func assets(matching query: TimelineQuery, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        let all = matching(query)
        let start = min(max(0, offset), all.count)
        let end = min(start + max(0, limit), all.count)
        return Page(
            items: Array(all[start ..< end]),
            request: PageRequest(offset: offset, limit: limit),
            totalCount: all.count
        )
    }

    func dayCounts(matching query: TimelineQuery) async throws -> [DayCount] {
        var order: [DayKey] = []
        var counts: [DayKey: Int] = [:]
        for asset in matching(query) {
            let key = asset.dayKey
            if counts[key] == nil { order.append(key) }
            counts[key, default: 0] += 1
        }
        return order.map { DayCount(dayKey: $0, count: counts[$0] ?? 0) }
    }

    func assetCount(matching query: TimelineQuery) async throws -> Int {
        matching(query).count
    }

    func asset(for id: AssetID) async throws -> LibraryAsset? {
        assets.first { $0.id == id }
    }

    func assets(for ids: [AssetID]) async throws -> [LibraryAsset] {
        ids.compactMap { id in assets.first { $0.id == id } }
    }

    func sidecar(for _: AssetID) async throws -> SidecarV1? { nil }

    func provenanceChain(for _: AssetID) async throws -> [ProvenanceRecord] { [] }

    nonisolated func changes() -> AsyncStream<LibraryChange> {
        AsyncStream { continuation in
            Task { await self.store(continuation) }
        }
    }

    private func store(_ continuation: AsyncStream<LibraryChange>.Continuation) {
        self.continuation = continuation
    }

    func emit(_ change: LibraryChange) {
        continuation?.yield(change)
    }
}

/// A second implementation of the same port, to prove the seam actually swaps.
private struct EmptyLibrary: LibraryPort {
    func assets(matching _: TimelineQuery, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        .empty(request: PageRequest(offset: offset, limit: limit), totalCount: 0)
    }

    func dayCounts(matching _: TimelineQuery) async throws -> [DayCount] { [] }
    func assetCount(matching _: TimelineQuery) async throws -> Int { 0 }
    func asset(for _: AssetID) async throws -> LibraryAsset? { nil }
    func assets(for _: [AssetID]) async throws -> [LibraryAsset] { [] }
    func sidecar(for _: AssetID) async throws -> SidecarV1? { nil }
    func provenanceChain(for _: AssetID) async throws -> [ProvenanceRecord] { [] }
    func changes() -> AsyncStream<LibraryChange> { AsyncStream { $0.finish() } }
}

/// A port that always fails, so a consumer's error path can be exercised.
private struct FailingQuota: QuotaPort {
    func status() async throws -> QuotaStatus {
        throw CapsuleError(code: .quotaExceeded, detail: "over the hard limit", httpStatus: 403)
    }

    func wouldAdmit(additionalBytes _: UInt64) async throws -> Bool { false }
    func changes() -> AsyncStream<QuotaStatus> { AsyncStream { $0.finish() } }
}

// MARK: - Fixtures

private enum PortFixtures {
    static func asset(_ id: String, captureSeconds: Int64, isDeleted: Bool = false) -> LibraryAsset {
        LibraryAsset(
            id: .managed(uuid: id),
            mediaType: .photo,
            contentType: .heic,
            captureTime: CaptureTime(
                captureTimestamp: CapsuleTimestamp(epochSeconds: captureSeconds),
                captureUTC: CapsuleTimestamp(epochSeconds: captureSeconds)
            ),
            importTimestamp: CapsuleTimestamp(epochSeconds: captureSeconds),
            isDeleted: isDeleted
        )
    }

    /// Three assets on 2026-01-02 and two on 2026-01-01.
    static let library: [LibraryAsset] = {
        let jan1 = Int64(1767225600)
        let jan2 = jan1 + 86400
        return [
            asset("a", captureSeconds: jan2 + 300),
            asset("b", captureSeconds: jan2 + 200),
            asset("c", captureSeconds: jan2 + 100),
            asset("d", captureSeconds: jan1 + 200),
            asset("e", captureSeconds: jan1 + 100),
        ]
    }()
}

// MARK: - Tests

@Suite("LibraryPort's shape supports a virtualized grid")
struct LibraryPortContractTests {
    @Test("an actor can conform, so the real adapter can hold connection state")
    func actorConformance() async throws {
        let library: any LibraryPort = StubLibrary(assets: PortFixtures.library)
        let page = try await library.assets(matching: .default, offset: 0, limit: 10)
        #expect(page.items.count == 5)
    }

    @Test("paging walks the timeline without ever materialising it")
    func pagingWalksTheTimeline() async throws {
        let library: any LibraryPort = StubLibrary(assets: PortFixtures.library)

        var collected: [LibraryAsset] = []
        var request: PageRequest? = PageRequest(offset: 0, limit: 2)
        while let current = request {
            let page = try await library.assets(
                matching: .default,
                offset: current.offset,
                limit: current.limit
            )
            collected += page.items
            request = page.nextRequest
        }

        #expect(collected.map(\.stableSortKey).count == 5)
        // Newest first, per the canonical order.
        #expect(collected.first?.id == .managed(uuid: "a"))
        #expect(collected.last?.id == .managed(uuid: "e"))
    }

    @Test("day counts turn into the section offsets a grid needs")
    func dayCountsGiveSectionOffsets() async throws {
        let library: any LibraryPort = StubLibrary(assets: PortFixtures.library)
        let counts = try await library.dayCounts(matching: .default)

        let total = try await library.assetCount(matching: .default)
        #expect(counts.map(\.count) == [3, 2])
        #expect(counts.sectionOffsets == [0, 3])
        #expect(counts.totalCount == total)
    }

    @Test("day counts and the paged read agree on the query's filters")
    func aggregateAgreesWithPages() async throws {
        // If the aggregate counted trashed assets and the page did not, every
        // section below the first would be offset by the difference — a grid
        // that scrolls to the wrong photo.
        var assets = PortFixtures.library
        assets.append(PortFixtures.asset("trashed", captureSeconds: 1767225600 + 400, isDeleted: true))
        let library: any LibraryPort = StubLibrary(assets: assets)

        let counts = try await library.dayCounts(matching: .default)
        let page = try await library.assets(matching: .default, offset: 0, limit: 100)
        #expect(counts.totalCount == page.items.count)

        // The Trash view is a *slice*, not the library with the trash mixed in.
        let trashCounts = try await library.dayCounts(matching: .trash)
        #expect(trashCounts.totalCount == 1)
    }

    @Test("swapping the implementation is a one-line change at the composition root")
    func implementationSwaps() async throws {
        // Feature code depends only on `any LibraryPort`, so the mock and the
        // eventual UniFFI adapter are interchangeable without touching a view.
        func firstAssetID(from port: any LibraryPort) async throws -> AssetID? {
            try await port.assets(matching: .default, offset: 0, limit: 1).items.first?.id
        }

        let stub: any LibraryPort = StubLibrary(assets: PortFixtures.library)
        let empty: any LibraryPort = EmptyLibrary()

        #expect(try await firstAssetID(from: stub) == .managed(uuid: "a"))
        #expect(try await firstAssetID(from: empty) == nil)
    }

    @Test("change notifications arrive as an AsyncStream, matching AssetProvider")
    func changesStream() async throws {
        let library = StubLibrary(assets: PortFixtures.library)
        let stream = library.changes()
        var iterator = stream.makeAsyncIterator()

        // Give the stream's stored continuation a chance to register before
        // emitting; the stub wires it up in a detached task.
        try await Task.sleep(for: .milliseconds(50))
        await library.emit(.dayCountsChanged)

        let change = await iterator.next()
        #expect(change == .dayCountsChanged)
    }
}

@Suite("Ports throw CapsuleError, so one error path serves every surface")
struct PortErrorContractTests {
    @Test("a failing port surfaces a coded error with its recovery attached")
    func codedErrors() async {
        let quota: any QuotaPort = FailingQuota()
        do {
            _ = try await quota.status()
            Issue.record("the stub must throw")
        } catch let error as CapsuleError {
            #expect(error.code == .quotaExceeded)
            #expect(error.recoveryAction == .surfaceToUser)
            #expect(error.localizationKey == "error.quota.exceeded")
            #expect(error.httpStatus == 403)
        } catch {
            Issue.record("expected a CapsuleError, got \(error)")
        }
    }

    @Test("a port that cannot admit a write says so without throwing")
    func predicateWithoutThrowing() async throws {
        // Asking "would this be admitted?" is a normal question with a normal
        // answer; only the attempt itself is an error path.
        let quota: any QuotaPort = FailingQuota()
        #expect(try await quota.wouldAdmit(additionalBytes: 1) == false)
    }
}
