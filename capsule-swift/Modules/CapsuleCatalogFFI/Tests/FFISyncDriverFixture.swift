import Foundation

@testable import CapsuleCatalogFFI

/// The doubles and builders the ``FFISyncDriver`` suites share. A namespace
/// rather than file-scope declarations, so the suites nested in it can sit in
/// their own files without their helpers colliding with the file-private ones
/// other suites of this module declare under the same names.
enum FFISyncDriverFixture {
    static let assetA = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
    static let assetB = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c"
    static let assetC = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5d"

    static let originalHash = String(repeating: "a", count: 64)
    static let metadataHash = String(repeating: "b", count: 64)

    /// One call of ``FFISyncDriverVerbs/applySyncEntry(_:ciphertextAt:)``, with
    /// the head's fields that decide its verdict.
    struct ApplyCall: Equatable {
        let manifest: Data
        let metadata: Data
        let localChainHead: Data?
        let ciphertext: Data
    }

    /// One call of ``FFISyncDriverVerbs/applyHeldSyncEntry(_:)``: the head's
    /// fields that decide its verdict, and no ciphertext, since none is read.
    struct HeldApplyCall: Equatable {
        let manifest: Data
        let metadata: Data
        let localChainHead: Data?
    }

    /// The plaintext the double opens every original to.
    static let plaintextBytes = Data("verified plaintext".utf8)
}

extension FFISyncDriverFixture {
    /// A session double: serves scripted feed pages in order, writes each fetched
    /// blob as its hash's bytes and answers each metadata blob fetch with its
    /// hash's bytes (or the scripted failure), answers each apply or open with the verdict
    /// scripted for the manifest, and records every call. An open that answers
    /// `opened` writes ``plaintextBytes`` at its destination, as the workspace does.
    /// A manifest's history is the one scripted for it, else none (a `create`'s);
    /// the blobs its history names are the ones scripted for it, else
    /// ``defaultSources``; an open against a history adopts unless scripted otherwise, and an asset
    /// adopted by either open is held from then on, its chain head the entry's.
    /// A delete's retention window is closed only for the manifests scripted as
    /// closed, and a held apply answers the verdict scripted for its manifest,
    /// else an adopted `delete`.
    final class ScriptedVerbs: FFISyncDriverVerbs, @unchecked Sendable {
        private let lock = NSLock()
        private var pages: [Result<FfiSyncPage, any Error>]
        private var pulledFrom: [Data] = []
        private var fetched: [String] = []
        private var applyCalls: [ApplyCall] = []
        private var heldApplyCalls: [HeldApplyCall] = []
        private var openCalls: [ApplyCall] = []
        private var historyFetchCalls: [Data] = []
        private var historyOpenCalls: [HistoryOpenCall] = []
        private var sourceCalls: [SourcesCall] = []
        private var retentionCalls: [RetentionCall] = []
        private var blobSizes: [UInt64] = []
        private let verdicts: [Data: FfiSyncApplyOutcome]
        private let openings: [Data: FfiOpenedOriginal]
        private let histories: [Data: Result<[Data], any Error>]
        private let historyOpenings: [Data: FfiOpenedOriginal]
        private let sources: [Data: Result<FfiHistorySources, any Error>]
        private let closedWindows: Set<Data>
        private var chainHeads: [String: Data]
        private let metadataFailure: (any Error)?

        init(
            pages: [Result<FfiSyncPage, any Error>],
            verdicts: [Data: FfiSyncApplyOutcome] = [:],
            openings: [Data: FfiOpenedOriginal] = [:],
            histories: [Data: Result<[Data], any Error>] = [:],
            historyOpenings: [Data: FfiOpenedOriginal] = [:],
            sources: [Data: Result<FfiHistorySources, any Error>] = [:],
            closedWindows: Set<Data> = [],
            chainHeads: [String: Data] = [:],
            metadataFailure: (any Error)? = nil
        ) {
            self.metadataFailure = metadataFailure
            self.pages = pages
            self.verdicts = verdicts
            self.openings = openings
            self.histories = histories
            self.historyOpenings = historyOpenings
            self.sources = sources
            self.closedWindows = closedWindows
            self.chainHeads = chainHeads
        }

        var opens: [ApplyCall] {
            lock.withLock { openCalls }
        }

        /// The manifests whose history was fetched, in order.
        var historyFetches: [Data] {
            lock.withLock { historyFetchCalls }
        }

        var historyOpens: [HistoryOpenCall] {
            lock.withLock { historyOpenCalls }
        }

        /// The records each history-sources naming was asked about, in order.
        var sourceNamings: [SourcesCall] {
            lock.withLock { sourceCalls }
        }

        /// The records whose retention window was judged, in order.
        var retentionJudgements: [RetentionCall] {
            lock.withLock { retentionCalls }
        }

        /// The size each blob fetch was asked for, in order.
        var fetchedSizes: [UInt64] {
            lock.withLock { blobSizes }
        }

        var cursors: [Data] {
            lock.withLock { pulledFrom }
        }

        var fetches: [String] {
            lock.withLock { fetched }
        }

        var applies: [ApplyCall] {
            lock.withLock { applyCalls }
        }

        var heldApplies: [HeldApplyCall] {
            lock.withLock { heldApplyCalls }
        }

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

        func fetchBlob(hash: String, size: UInt64, to path: URL) throws {
            lock.withLock {
                fetched.append(hash)
                blobSizes.append(size)
            }
            try Data(hash.utf8).write(to: path)
        }

        func fetchMetadataBlob(hash: String) throws -> Data {
            lock.withLock { fetched.append(hash) }
            if let metadataFailure { throw metadataFailure }
            return Data(hash.utf8)
        }

        func fetchProvenanceHistory(manifestCbor: Data) throws -> [Data] {
            try lock.withLock {
                historyFetchCalls.append(manifestCbor)
                return try histories[manifestCbor]?.get() ?? []
            }
        }

        func historySources(manifestCbor: Data, history: [Data]) throws -> FfiHistorySources {
            try lock.withLock {
                sourceCalls.append(SourcesCall(manifest: manifestCbor, history: history))
                return try sources[manifestCbor]?.get() ?? defaultSources
            }
        }

        func retentionClosed(manifestCbor: Data, changedAt: String) -> Bool {
            lock.withLock {
                retentionCalls.append(RetentionCall(manifest: manifestCbor, changedAt: changedAt))
                return closedWindows.contains(manifestCbor)
            }
        }

        func applySyncEntry(_ head: FfiSyncHead, ciphertextAt path: URL) throws -> FfiSyncApplyOutcome {
            let ciphertext = try Data(contentsOf: path)
            return lock.withLock {
                applyCalls.append(ApplyCall(
                    manifest: head.manifestCbor,
                    metadata: head.metadataBlob,
                    localChainHead: head.localChainHead,
                    ciphertext: ciphertext
                ))
                return verdicts[head.manifestCbor] ?? .applied(facts: facts(head.manifestCbor))
            }
        }

        func applyHeldSyncEntry(_ head: FfiSyncHead) -> FfiSyncApplyOutcome {
            lock.withLock {
                heldApplyCalls.append(HeldApplyCall(
                    manifest: head.manifestCbor,
                    metadata: head.metadataBlob,
                    localChainHead: head.localChainHead
                ))
                let outcome = verdicts[head.manifestCbor]
                    ?? .applied(facts: facts(head.manifestCbor, action: "delete", adopted: true))
                if case let .applied(facts) = outcome, facts.adopted {
                    chainHeads[facts.assetId] = head.manifestCbor
                }
                return outcome
            }
        }

        func openSyncedOriginal(_ head: FfiSyncHead, ciphertextAt path: URL, to destination: URL) throws -> FfiOpenedOriginal {
            let ciphertext = try Data(contentsOf: path)
            let opened = lock.withLock {
                openCalls.append(ApplyCall(
                    manifest: head.manifestCbor,
                    metadata: head.metadataBlob,
                    localChainHead: head.localChainHead,
                    ciphertext: ciphertext
                ))
                let opened = openings[head.manifestCbor] ?? .opened(facts: facts(head.manifestCbor, metadata: metadataFacts))
                hold(opened, head: head)
                return opened
            }
            if case .opened = opened {
                try plaintextBytes.write(to: destination)
            }
            return opened
        }

        func openSyncedHistory(
            _ head: FfiSyncHead,
            history: [Data],
            ciphertextAt path: URL,
            to destination: URL
        ) throws -> FfiOpenedOriginal {
            let ciphertext = try Data(contentsOf: path)
            let opened = lock.withLock {
                historyOpenCalls.append(HistoryOpenCall(
                    call: ApplyCall(
                        manifest: head.manifestCbor,
                        metadata: head.metadataBlob,
                        localChainHead: head.localChainHead,
                        ciphertext: ciphertext
                    ),
                    history: history
                ))
                let opened = historyOpenings[head.manifestCbor]
                    ?? .opened(facts: facts(head.manifestCbor, action: "metadata-update", metadata: metadataFacts, adopted: true))
                hold(opened, head: head)
                return opened
            }
            if case .opened = opened {
                try plaintextBytes.write(to: destination)
            }
            return opened
        }

        /// Hold the asset an `opened` verdict adopted, as the workspace does.
        /// Called under `lock`.
        private func hold(_ opened: FfiOpenedOriginal, head: FfiSyncHead) {
            if case let .opened(facts) = opened, facts.adopted {
                chainHeads[facts.assetId] = head.manifestCbor
            }
        }

        func provenanceHead(assetId: String) throws -> Data {
            try lock.withLock {
                guard let head = chainHeads[assetId] else {
                    throw FfiError.Workspace(detail: "asset \(assetId) is not managed by this workspace")
                }
                return head
            }
        }

        func assetIds() -> [String] {
            lock.withLock { Array(chainHeads.keys) }
        }
    }
}

extension FFISyncDriverFixture {
    /// One call of ``FFISyncDriverVerbs/openSyncedHistory(_:history:ciphertextAt:to:)``.
    struct HistoryOpenCall: Equatable {
        let call: ApplyCall
        let history: [Data]
    }

    /// One call of ``FFISyncDriverVerbs/historySources(manifestCbor:history:)``.
    struct SourcesCall: Equatable {
        let manifest: Data
        let history: [Data]
    }

    /// One call of ``FFISyncDriverVerbs/retentionClosed(manifestCbor:changedAt:)``.
    struct RetentionCall: Equatable {
        let manifest: Data
        let changedAt: String
    }

    /// The blobs a history names unless scripted otherwise: the feed's own
    /// metadata blob and original.
    static let defaultSources = FfiHistorySources(
        metadataBlobHash: metadataHash,
        original: FfiBlobRef(ciphertextHash: originalHash, role: "original", size: 64)
    )

    /// An in-memory ``FFISyncStateStoring`` that counts its saves.
    final class MemoryStore: FFISyncStateStoring, @unchecked Sendable {
        private let lock = NSLock()
        private var saved: FFISyncState?
        private var saveCount = 0

        init(_ state: FFISyncState? = nil) {
            saved = state
        }

        var state: FFISyncState? {
            lock.withLock { saved }
        }

        var saves: Int {
            lock.withLock { saveCount }
        }

        func load() -> FFISyncState? {
            state
        }

        func save(_ state: FFISyncState) {
            lock.withLock {
                saved = state
                saveCount += 1
            }
        }
    }

    /// Collects every drain the driver hands over.
    actor DrainSink {
        private(set) var drains: [FFISyncDrain] = []

        func record(_ drain: FFISyncDrain) {
            drains.append(drain)
        }
    }

    /// One write the catalog double was handed, with what the staged plaintext
    /// held at that moment.
    struct CatalogCall: Equatable {
        let action: String
        let hasMetadata: Bool
        /// The plaintext's bytes, `nil` when no original was handed over.
        let original: Data?
        let originalPath: URL?
    }

    /// A catalog writer double: records each write and answers it as scripted,
    /// throwing for the asset ids it is told to.
    final class RecordingCatalog: FFISyncCatalogWriting, @unchecked Sendable {
        private let lock = NSLock()
        private var calls: [CatalogCall] = []
        private let failing: Set<String>

        init(failing: Set<String> = []) {
            self.failing = failing
        }

        var writes: [CatalogCall] {
            lock.withLock { calls }
        }

        func write(_ facts: FfiAssetFacts, original: URL?) throws -> FFISyncCatalogWrite {
            let bytes = try original.map { try Data(contentsOf: $0) }
            lock.withLock {
                calls.append(CatalogCall(
                    action: facts.action,
                    hasMetadata: facts.metadata != nil,
                    original: bytes,
                    originalPath: original
                ))
            }
            if failing.contains(facts.assetId) {
                throw FfiError.Workspace(detail: "catalog write refused")
            }
            return facts.metadata == nil ? .unwritten : .upserted(placedOriginal: original != nil)
        }
    }
}

extension FFISyncDriverFixture {
    /// The metadata an opened entry carries.
    static let metadataFacts = FfiAssetMetadata(
        sidecarSchema: 1,
        contentType: "image/jpeg",
        captureTimestamp: "2026-01-01T00:00:00Z",
        width: 4,
        height: 3,
        lqipChromahash: nil,
        lqipDominantColor: nil,
        caption: nil,
        tags: [],
        rating: nil,
        hidden: false
    )

    /// The facts the double answers an applied entry with: its provenance head is
    /// the record hash of `manifest`, as the chokepoint's is.
    static func facts(
        _ manifest: Data,
        action: String = "create",
        metadata: FfiAssetMetadata? = nil,
        adopted: Bool = false
    ) -> FfiAssetFacts {
        FfiAssetFacts(
            assetId: assetA,
            albumId: assetA,
            amkVersion: 1,
            ciphertextHash: originalHash,
            plaintextSize: 1,
            action: action,
            timestamp: "2026-01-01T00:00:00Z",
            createdByDevice: assetA,
            provenanceHead: FFIPurgeDelivery.headHash(manifest),
            metadata: metadata,
            adopted: adopted
        )
    }

    /// A feed entry listing `asset` with `record` as its latest record, carrying
    /// an original the server holds unless `held` is false. As the server's feed
    /// does, it names its metadata blob (`metadataAddress`, when `metadata` is
    /// set) by address only, and lists only the original among its blobs.
    static func entry(
        _ asset: String,
        record: Data,
        kind: FfiChangeKind = .created,
        metadata: Bool = false,
        held: Bool = true,
        metadataAddress: Data = Data(metadataHash.utf8)
    ) -> FfiFeedEntry {
        FfiFeedEntry(
            albumId: Data(repeating: 0xAB, count: 16),
            syncSeq: 1,
            protocolVersion: "2026-01-01",
            kind: kind,
            assetId: Data(asset.utf8),
            manifestCbor: record,
            metadataBlob: metadata ? metadataAddress : Data(),
            blobs: FfiBlobManifest(
                original: FfiBlobRef(ciphertextHash: originalHash, role: "original", size: 64),
                derivatives: []
            ),
            originalHeld: held,
            changedAt: "2026-01-01T00:00:00Z"
        )
    }

    /// The `changed_at` every scripted tombstone carries.
    static let tombstoneChangedAt = "2026-02-01T00:00:00Z"

    /// An album page's tombstone of `asset`: its delete `record`, and, as every
    /// album page serves a delete, no metadata blob, no blob and no held original.
    static func tombstone(_ asset: String, record: Data) -> FfiFeedEntry {
        FfiFeedEntry(
            albumId: Data(repeating: 0xAB, count: 16),
            syncSeq: 1,
            protocolVersion: "2026-01-01",
            kind: .deleted,
            assetId: Data(asset.utf8),
            manifestCbor: record,
            metadataBlob: Data(),
            blobs: FfiBlobManifest(original: nil, derivatives: []),
            originalHeld: false,
            changedAt: tombstoneChangedAt
        )
    }

    static let liveMetadataHash = String(repeating: "e", count: 64)
    static let liveOriginalHash = String(repeating: "f", count: 64)

    /// The blobs a tombstone's history names: its asset's live metadata blob and
    /// the original its delete commits to, neither of which the feed lists.
    static let liveSources = FfiHistorySources(
        metadataBlobHash: liveMetadataHash,
        original: FfiBlobRef(ciphertextHash: liveOriginalHash, role: "original", size: 80)
    )

    static func page(_ entries: [FfiFeedEntry], next: Data, more: Bool = false) -> FfiSyncPage {
        FfiSyncPage(entries: entries, nextCursor: next, hasMore: more)
    }

    static func stagingDirectory() -> URL {
        URL.temporaryDirectory.appending(path: "ffi-sync-\(UUID().uuidString)", directoryHint: .isDirectory)
    }

    static func driver(
        _ verbs: ScriptedVerbs,
        store: MemoryStore = MemoryStore(),
        catalog: RecordingCatalog = RecordingCatalog(),
        staging: URL = stagingDirectory(),
        sink: DrainSink
    ) -> FFISyncDriver {
        FFISyncDriver(verbs: verbs, store: store, stagingDirectory: staging, catalog: catalog) { await sink.record($0) }
    }

    /// The files left in `directory`, hidden ones included.
    static func leftovers(in directory: URL) throws -> [URL] {
        try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
    }
}
