@testable import CapsuleCatalogFFI
import Foundation

/// The doubles and builders the ``FFIPurgeDelivery`` suites share. A namespace
/// rather than file-scope declarations, so the suites nested in it can sit in
/// their own files without their helpers colliding with the file-private ones
/// other suites of this module declare under the same names.
enum FFIPurgeDeliveryFixture {
    /// A purged asset's id as the workspace names it, and the same id as another
    /// device's feed may spell it.
    static let assetA = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
    static let assetB = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c"

    /// One purge the session double was handed.
    struct Delivery: Equatable {
        let assetID: String
        let serverHeadHash: Data?
    }

    /// One lifecycle delivery the session double was handed.
    struct LifecycleCall: Equatable {
        let assetID: String
        let serverHeadHash: Data?
        let latestChangedAt: String?
    }

    /// A session double: serves scripted feed pages in order and answers each
    /// delivery with the outcome scripted for its asset, recording every call.
    final class ScriptedVerbs: FFIPurgeDeliveryVerbs, @unchecked Sendable {
        private let lock = NSLock()
        private var pages: [Result<FfiSyncPage, any Error>]
        private var pulledFrom: [Data] = []
        private var delivered: [Delivery] = []
        private var lifecycleCalls: [LifecycleCall] = []
        private let outcomes: [String: FfiLifecycleOpOutcome]
        private let purged: [String]
        private let keptHeads: [String: Data]
        private let held: [String]
        private let chainHeads: [String: Data]

        init(
            pages: [Result<FfiSyncPage, any Error>],
            outcomes: [String: FfiLifecycleOpOutcome] = [:],
            purged: [String] = [],
            keptHeads: [String: Data] = [:],
            held: [String] = [],
            chainHeads: [String: Data] = [:]
        ) {
            self.pages = pages
            self.outcomes = outcomes
            self.purged = purged
            self.keptHeads = keptHeads
            self.held = held
            self.chainHeads = chainHeads
        }

        var cursors: [Data] {
            lock.withLock { pulledFrom }
        }

        var deliveries: [Delivery] {
            lock.withLock { delivered }
        }

        var lifecycleDeliveries: [LifecycleCall] {
            lock.withLock { lifecycleCalls }
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

        func deliverLifecycle(assetId: String, serverHeadHash: Data?, latestChangedAt: String?) -> FfiLifecycleOpOutcome {
            lock.withLock {
                lifecycleCalls.append(LifecycleCall(
                    assetID: assetId,
                    serverHeadHash: serverHeadHash,
                    latestChangedAt: latestChangedAt
                ))
                return outcomes[assetId] ?? .notIndexed
            }
        }

        func provenanceHead(assetId: String) throws -> Data {
            guard let head = chainHeads[assetId] else {
                throw FfiError.Workspace(detail: "asset \(assetId) is not managed by this workspace")
            }
            return head
        }

        func assetIds() -> [String] {
            held
        }
    }

    /// A feed entry listing `asset` (as UTF-8 bytes of its id) with `head` as its
    /// latest record, changed at `changedAt` on the server's clock.
    static func entry(
        _ asset: String,
        head: Data,
        seq: UInt64 = 1,
        changedAt: String = "2026-01-01T00:00:00Z"
    ) -> FfiFeedEntry {
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
            changedAt: changedAt
        )
    }

    /// The record hash the delivery keeps and sends for a listed head of `bytes`.
    static func hashed(_ bytes: UInt8...) -> Data {
        FFIPurgeDelivery.headHash(Data(bytes))
    }

    static func page(_ entries: [FfiFeedEntry], next: Data, more: Bool = false) -> FfiSyncPage {
        FfiSyncPage(entries: entries, nextCursor: next, hasMore: more)
    }

    static let applied = FfiLifecycleOpOutcome.applied(assetId: assetA, action: "delete", syncSeq: 9, replayed: false)

    static func failure(permanent: Bool) -> FfiLifecycleOpOutcome {
        .failed(
            kind: permanent ? "stale_revival" : "unavailable",
            code: nil,
            permanent: permanent,
            chainHead: nil,
            retentionUntil: nil,
            detail: "refused"
        )
    }
}
