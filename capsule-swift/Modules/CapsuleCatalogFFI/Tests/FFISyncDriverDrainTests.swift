import Foundation
import Testing

@testable import CapsuleCatalogFFI

private let firstAsset = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a6a"
private let secondAsset = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a6b"
private let originalHash = String(repeating: "c", count: 64)

/// A one-shot latch: `wait()` suspends until `open()`, and returns at once after.
private actor Latch {
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        isOpen = true
        waiters.forEach { $0.resume() }
        waiters = []
    }
}

/// A session double whose every pull opens `entered` and then waits on
/// `release`, so a test holds a drain inside its first pull. It serves its
/// pages in order, writes each fetched blob as its hash's bytes, applies every
/// entry, and records each pull's cursor and each apply.
private final class GatedVerbs: FFISyncDriverVerbs, @unchecked Sendable {
    let entered = Latch()
    let release = Latch()
    private let lock = NSLock()
    private var pages: [FfiSyncPage]
    private var pulledFrom: [Data] = []
    private var applyCount = 0

    init(pages: [FfiSyncPage]) {
        self.pages = pages
    }

    var cursors: [Data] {
        lock.withLock { pulledFrom }
    }

    var applies: Int {
        lock.withLock { applyCount }
    }

    func syncPull(cursor: Data, pageSize _: UInt32) async throws -> FfiSyncPage {
        await entered.open()
        await release.wait()
        return lock.withLock {
            pulledFrom.append(cursor)
            guard !pages.isEmpty else { return FfiSyncPage(entries: [], nextCursor: cursor, hasMore: false) }
            return pages.removeFirst()
        }
    }

    func fetchBlob(hash: String, size _: UInt64, to path: URL) throws {
        try Data(hash.utf8).write(to: path)
    }

    func applySyncEntry(_ head: FfiSyncHead, ciphertextAt _: URL) -> FfiSyncApplyOutcome {
        lock.withLock { applyCount += 1 }
        return .applied(facts: FfiAssetFacts(
            assetId: firstAsset,
            albumId: firstAsset,
            amkVersion: 1,
            ciphertextHash: originalHash,
            plaintextSize: 1,
            action: "create",
            timestamp: "2026-01-01T00:00:00Z",
            createdByDevice: firstAsset,
            provenanceHead: FFIPurgeDelivery.headHash(head.manifestCbor),
            metadata: nil
        ))
    }

    func openSyncedOriginal(_: FfiSyncHead, ciphertextAt _: URL, to _: URL) -> FfiOpenedOriginal {
        .refused(outcome: .quarantined(reason: "MalformedManifest", detail: nil))
    }

    func provenanceHead(assetId: String) throws -> Data {
        throw FfiError.Workspace(detail: "asset \(assetId) is not managed by this workspace")
    }

    func assetIds() -> [String] {
        []
    }
}

/// A catalog writer that writes nothing: these drains carry no metadata.
private struct NoCatalog: FFISyncCatalogWriting {
    func write(_: FfiAssetFacts, original _: URL?) -> FFISyncCatalogWrite {
        .unwritten
    }
}

/// An in-memory ``FFISyncStateStoring`` that counts its saves.
private final class CountingStore: FFISyncStateStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var saveCount = 0

    var saves: Int {
        lock.withLock { saveCount }
    }

    func load() -> FFISyncState? {
        nil
    }

    func save(_: FFISyncState) {
        lock.withLock { saveCount += 1 }
    }
}

/// Counts the drains the driver hands over.
private actor DrainCounter {
    private(set) var drains = 0

    func record(_: FFISyncDrain) {
        drains += 1
    }
}

/// A created entry listing `asset` with `record` as its latest record and an
/// original the server holds.
private func entry(_ asset: String, record: Data) -> FfiFeedEntry {
    FfiFeedEntry(
        albumId: Data(repeating: 0xAB, count: 16),
        syncSeq: 1,
        protocolVersion: "2026-01-01",
        kind: .created,
        assetId: Data(asset.utf8),
        manifestCbor: record,
        metadataBlob: Data(),
        blobs: FfiBlobManifest(
            original: FfiBlobRef(ciphertextHash: originalHash, role: "original", size: 64),
            derivatives: []
        ),
        originalHeld: true,
        changedAt: "2026-01-01T00:00:00Z"
    )
}

/// Two pages: the first lists `firstAsset` and claims more, the second lists
/// `secondAsset` and ends the feed.
private func twoPages() -> [FfiSyncPage] {
    [
        FfiSyncPage(entries: [entry(firstAsset, record: Data([1]))], nextCursor: Data([1]), hasMore: true),
        FfiSyncPage(entries: [entry(secondAsset, record: Data([2]))], nextCursor: Data([2]), hasMore: false),
    ]
}

private func stagingDirectory() -> URL {
    URL.temporaryDirectory.appending(path: "ffi-sync-drain-\(UUID().uuidString)", directoryHint: .isDirectory)
}

@Suite("FFISyncDriver — one drain at a time, over a clean staging directory")
struct FFISyncDriverDrainTests {
    @Test("a sync asked for while a drain runs joins it: each page is pulled, applied and saved once")
    func concurrentSyncJoinsTheDrain() async throws {
        let verbs = GatedVerbs(pages: twoPages())
        let store = CountingStore()
        let counter = DrainCounter()
        let subject = FFISyncDriver(verbs: verbs, store: store, stagingDirectory: stagingDirectory(), catalog: NoCatalog()) {
            await counter.record($0)
        }

        let first = Task { try await subject.sync() }
        await verbs.entered.wait()
        let second = Task { try await subject.sync() }
        var spins = 0
        while await subject.joins == 0, spins < 100000 {
            spins += 1
            await Task.yield()
        }
        try #require(await subject.joins == 1)
        await verbs.release.open()

        let firstDrain = try await first.value
        let secondDrain = try await second.value

        #expect(firstDrain == secondDrain)
        #expect(firstDrain.pages == 2)
        #expect(firstDrain.newlyIndexed == [firstAsset, secondAsset])
        #expect(verbs.cursors == [Data(), Data([1])])
        #expect(verbs.applies == 2)
        #expect(store.saves == 2)
        #expect(await counter.drains == 1)
    }

    @Test("a drain first removes the staged files a killed drain left")
    func drainClearsLeftoverStaging() async throws {
        let staging = stagingDirectory()
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        let leftover = staging.appending(path: UUID().uuidString)
        try Data("half-fetched original".utf8).write(to: leftover)
        let verbs = GatedVerbs(pages: twoPages())
        await verbs.release.open()
        let subject = FFISyncDriver(verbs: verbs, store: CountingStore(), stagingDirectory: staging, catalog: NoCatalog()) { _ in }

        try await subject.sync()

        #expect(!FileManager.default.fileExists(atPath: leftover.path))
        #expect(try FileManager.default.contentsOfDirectory(atPath: staging.path).isEmpty)
    }
}
