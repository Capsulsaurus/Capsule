import CapsuleCatalog
import Foundation
import Testing

@testable import CapsuleCatalogFFI

/// A purged asset's id as the workspace names it, and the same id as another
/// device's feed may spell it.
private let assetA = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
private let assetB = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c"

/// A session double: serves scripted feed pages in order and answers each
/// delivery with the outcome scripted for its asset, recording every call.
private final class ScriptedVerbs: FFIPurgeDeliveryVerbs, @unchecked Sendable {
    struct Delivery: Equatable {
        let assetID: String
        let serverHeadHash: Data?
    }

    private let lock = NSLock()
    private var pages: [Result<FfiSyncPage, any Error>]
    private var pulledFrom: [Data] = []
    private var delivered: [Delivery] = []
    private let outcomes: [String: FfiLifecycleOpOutcome]
    private let purged: [String]
    private let keptHeads: [String: Data]

    init(
        pages: [Result<FfiSyncPage, any Error>],
        outcomes: [String: FfiLifecycleOpOutcome] = [:],
        purged: [String] = [],
        keptHeads: [String: Data] = [:]
    ) {
        self.pages = pages
        self.outcomes = outcomes
        self.purged = purged
        self.keptHeads = keptHeads
    }

    var cursors: [Data] {
        lock.withLock { pulledFrom }
    }

    var deliveries: [Delivery] {
        lock.withLock { delivered }
    }

    /// Script the pages the next drains read.
    func enqueue(_ more: [Result<FfiSyncPage, any Error>]) {
        lock.withLock { pages += more }
    }

    func syncPull(cursor: Data, pageSize _: UInt32) throws -> FfiSyncPage {
        try lock.withLock {
            pulledFrom.append(cursor)
            guard !pages.isEmpty else { return page([], next: cursor) }
            return try pages.removeFirst().get()
        }
    }

    func deliverPurge(assetId: String, serverHeadHash: Data?) -> FfiLifecycleOpOutcome {
        lock.withLock {
            delivered.append(Delivery(assetID: assetId, serverHeadHash: serverHeadHash))
            return outcomes[assetId] ?? .notIndexed
        }
    }

    func purgedRecord(assetId: String) -> Data? {
        keptHeads[assetId]
    }

    func purgedAssetIds() -> [String] {
        purged
    }
}

/// A feed entry listing `asset` (as UTF-8 bytes of its id) with `head` as its
/// latest record.
private func entry(_ asset: String, head: Data, seq: UInt64 = 1) -> FfiFeedEntry {
    FfiFeedEntry(
        albumId: Data(repeating: 0xAB, count: 16),
        syncSeq: seq,
        protocolVersion: "2026-01-01",
        kind: .updated,
        assetId: Data(asset.utf8),
        manifestCbor: head,
        metadataBlob: Data(),
        blobs: FfiBlobManifest(original: nil, derivatives: []),
        originalHeld: true,
        changedAt: "2026-01-01T00:00:00Z"
    )
}

/// The record hash the delivery keeps and sends for a listed head of `bytes`.
private func hashed(_ bytes: UInt8...) -> Data {
    FFIPurgeDelivery.headHash(Data(bytes))
}

private func page(_ entries: [FfiFeedEntry], next: Data, more: Bool = false) -> FfiSyncPage {
    FfiSyncPage(entries: entries, nextCursor: next, hasMore: more)
}

private let applied = FfiLifecycleOpOutcome.applied(assetId: assetA, action: "delete", syncSeq: 9, replayed: false)

private func failure(permanent: Bool) -> FfiLifecycleOpOutcome {
    .failed(
        kind: permanent ? "stale_revival" : "unavailable",
        code: nil,
        permanent: permanent,
        chainHead: nil,
        retentionUntil: nil,
        detail: "refused"
    )
}

@Suite("FFIPurgeDelivery — a purge delivered with the head the sync feed names")
struct FFIPurgeDeliveryTests {
    @Test(
        "each session outcome becomes the PurgeDelivery it names",
        arguments: [
            (applied, PurgeDelivery.delivered),
            (FfiLifecycleOpOutcome.applied(assetId: assetA, action: "delete", syncSeq: 9, replayed: true), .delivered),
            (FfiLifecycleOpOutcome.notIndexed, .held),
            (failure(permanent: true), .failed(permanent: true)),
            (failure(permanent: false), .failed(permanent: false)),
        ]
    )
    func mapsOutcomes(outcome: FfiLifecycleOpOutcome, expected: PurgeDelivery) async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, head: Data([1]))], next: Data([1])))],
            outcomes: [assetA: outcome]
        )

        #expect(try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA) == expected)
    }

    @Test("the head sent is the latest entry's record across every page of the drain")
    func sendsTheLatestHead() async throws {
        let verbs = ScriptedVerbs(
            pages: [
                .success(page([entry(assetA, head: Data([0x01]), seq: 1)], next: Data([1]), more: true)),
                .success(page([entry(assetB, head: Data([0x02]), seq: 2)], next: Data([2]), more: true)),
                .success(page([entry(assetA, head: Data([0x03]), seq: 3)], next: Data([3]))),
            ],
            outcomes: [assetA: applied]
        )

        _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

        #expect(verbs.cursors == [Data(), Data([1]), Data([2])])
        #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: hashed(0x03))])
    }

    @Test("an asset the feed does not list is sent with no head, so the session holds it")
    func unlistedAssetHasNoHead() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page([entry(assetB, head: Data([0x02]))], next: Data([1])))])

        let delivery = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

        #expect(delivery == .held)
        #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: nil)])
    }

    @Test("a listed entry that carries no record is sent as an empty head, not as unlisted")
    func emptyHeadIsListed() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, head: Data())], next: Data([1])))])

        _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

        #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: Data())])
    }

    @Test("a purge whose kept head the feed already lists is delivered without a send")
    func levelPurgeSendsNothing() async throws {
        let kept = Data([0x09, 0x0A])
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, head: kept), entry(assetB, head: Data([0x01]))], next: Data([1])))],
            outcomes: [assetB: applied],
            keptHeads: [assetA: kept, assetB: Data([0x02])]
        )
        let delivery = FFIPurgeDelivery(verbs: verbs)

        #expect(try await delivery.deliverPurge(assetID: assetA) == .delivered)
        #expect(try await delivery.deliverPurge(assetID: assetB) == .delivered)

        // The server holds A's kept head, so only B, whose head it lacks, is sent.
        #expect(verbs.deliveries == [.init(assetID: assetB, serverHeadHash: hashed(0x01))])
    }

    @Test("a listed entry that carries no record is never taken as level")
    func emptyHeadIsNotLevel() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, head: Data())], next: Data([1])))],
            keptHeads: [assetA: Data()]
        )

        _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

        #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: Data())])
    }

    @Test("a head's hash is the SHA-256 of its bytes, 32 bytes whatever the record's size")
    func headHashIsSHA256() {
        let hex = FFIPurgeDelivery.headHash(Data("abc".utf8)).map { String(format: "%02x", $0) }.joined()
        #expect(hex == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        #expect(FFIPurgeDelivery.headHash(Data(repeating: 7, count: 4096)).count == 32)
    }

    @Test("a feed id in another case names the same asset; one that is not a UUID is ignored")
    func feedIdsAreNormalised() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page(
            [entry(assetA.uppercased(), head: Data([0x07])), entry("not-a-uuid", head: Data([0x08]))],
            next: Data([1])
        ))])

        _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)

        #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: hashed(0x07))])
        #expect(FFIPurgeDelivery.feedKey("not-a-uuid") == nil)
    }

    @Test("a later delivery resumes from the cursor the last drain ended at and folds what changed")
    func laterDeliveryResumes() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, head: Data([0x01]))], next: Data([1])))],
            outcomes: [assetA: applied]
        )
        let delivery = FFIPurgeDelivery(verbs: verbs)
        _ = try await delivery.deliverPurge(assetID: assetA)

        verbs.enqueue([.success(page([entry(assetA, head: Data([0x05]))], next: Data([2])))])
        _ = try await delivery.deliverPurge(assetID: assetA)

        #expect(verbs.cursors == [Data(), Data([1])])
        #expect(verbs.deliveries.map(\.serverHeadHash) == [hashed(0x01), hashed(0x05)])
    }

    @Test("a feed read that fails sends nothing, and the next drain starts where the last good one ended")
    func failedDrainCommitsNothing() async throws {
        let verbs = ScriptedVerbs(pages: [
            .success(page([entry(assetA, head: Data([0x01]))], next: Data([1]), more: true)),
            .failure(FfiError.Sync(code: nil, detail: "unreachable")),
        ])
        let delivery = FFIPurgeDelivery(verbs: verbs)

        await #expect(throws: FfiError.self) { _ = try await delivery.deliverPurge(assetID: assetA) }
        #expect(verbs.deliveries.isEmpty)

        verbs.enqueue([.success(page([entry(assetB, head: Data([0x02]))], next: Data([2])))])
        _ = try await delivery.deliverPurge(assetID: assetA)

        // The first page's head was read in the drain that failed, so it is not trusted.
        #expect(verbs.cursors == [Data(), Data([1]), Data()])
        #expect(verbs.deliveries == [.init(assetID: assetA, serverHeadHash: nil)])
    }

    @Test("a page that claims more without advancing the cursor stops the drain")
    func stalledCursorStops() async {
        let verbs = ScriptedVerbs(pages: [
            .success(page([], next: Data([1]), more: true)),
            .success(page([], next: Data([1]), more: true)),
        ])

        await #expect(throws: FeedDrainError.cursorStalled) {
            _ = try await FFIPurgeDelivery(verbs: verbs).deliverPurge(assetID: assetA)
        }
        #expect(verbs.deliveries.isEmpty)
    }

    @Test("the purged assets are the workspace's list, unchanged")
    func listsPurgedAssets() async throws {
        let verbs = ScriptedVerbs(pages: [], purged: [assetA, assetB])

        #expect(try await FFIPurgeDelivery(verbs: verbs).purgedAssetIDs() == [assetA, assetB])
        #expect(verbs.cursors.isEmpty)
    }
}
