import Foundation
import Testing

@testable import CapsuleCatalogFFI

private let assetA = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
private let assetB = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c"
private let assetC = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5d"

private let originalHash = String(repeating: "a", count: 64)
private let metadataHash = String(repeating: "b", count: 64)

/// One call of ``FFISyncDriverVerbs/applySyncEntry(_:ciphertextAt:)``, with
/// the head's fields that decide its verdict.
private struct ApplyCall: Equatable {
    let manifest: Data
    let metadata: Data
    let localChainHead: Data?
    let ciphertext: Data
}

/// The plaintext the double opens every original to.
private let plaintextBytes = Data("verified plaintext".utf8)

/// A session double: serves scripted feed pages in order, writes each fetched
/// blob as its hash's bytes and answers each metadata blob fetch with its
/// hash's bytes (or the scripted failure), answers each apply or open with the verdict
/// scripted for the manifest, and records every call. An open that answers
/// `opened` writes ``plaintextBytes`` at its destination, as the workspace does.
/// A manifest's history is the one scripted for it, else none (a `create`'s);
/// the blobs its history names are the ones scripted for it, else
/// ``defaultSources``; an open against a history adopts unless scripted otherwise, and an asset
/// adopted by either open is held from then on, its chain head the entry's.
private final class ScriptedVerbs: FFISyncDriverVerbs, @unchecked Sendable {
    private let lock = NSLock()
    private var pages: [Result<FfiSyncPage, any Error>]
    private var pulledFrom: [Data] = []
    private var fetched: [String] = []
    private var applyCalls: [ApplyCall] = []
    private var openCalls: [ApplyCall] = []
    private var historyFetchCalls: [Data] = []
    private var historyOpenCalls: [HistoryOpenCall] = []
    private var sourceCalls: [SourcesCall] = []
    private var blobSizes: [UInt64] = []
    private let verdicts: [Data: FfiSyncApplyOutcome]
    private let openings: [Data: FfiOpenedOriginal]
    private let histories: [Data: Result<[Data], any Error>]
    private let historyOpenings: [Data: FfiOpenedOriginal]
    private let sources: [Data: Result<FfiHistorySources, any Error>]
    private var chainHeads: [String: Data]
    private let metadataFailure: (any Error)?

    init(
        pages: [Result<FfiSyncPage, any Error>],
        verdicts: [Data: FfiSyncApplyOutcome] = [:],
        openings: [Data: FfiOpenedOriginal] = [:],
        histories: [Data: Result<[Data], any Error>] = [:],
        historyOpenings: [Data: FfiOpenedOriginal] = [:],
        sources: [Data: Result<FfiHistorySources, any Error>] = [:],
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

/// One call of ``FFISyncDriverVerbs/openSyncedHistory(_:history:ciphertextAt:to:)``.
private struct HistoryOpenCall: Equatable {
    let call: ApplyCall
    let history: [Data]
}

/// One call of ``FFISyncDriverVerbs/historySources(manifestCbor:history:)``.
private struct SourcesCall: Equatable {
    let manifest: Data
    let history: [Data]
}

/// The blobs a history names unless scripted otherwise: the feed's own
/// metadata blob and original.
private let defaultSources = FfiHistorySources(
    metadataBlobHash: metadataHash,
    original: FfiBlobRef(ciphertextHash: originalHash, role: "original", size: 64)
)

/// An in-memory ``FFISyncStateStoring`` that counts its saves.
private final class MemoryStore: FFISyncStateStoring, @unchecked Sendable {
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
private actor DrainSink {
    private(set) var drains: [FFISyncDrain] = []

    func record(_ drain: FFISyncDrain) {
        drains.append(drain)
    }
}

/// One write the catalog double was handed, with what the staged plaintext
/// held at that moment.
private struct CatalogCall: Equatable {
    let action: String
    let hasMetadata: Bool
    /// The plaintext's bytes, `nil` when no original was handed over.
    let original: Data?
    let originalPath: URL?
}

/// A catalog writer double: records each write and answers it as scripted,
/// throwing for the asset ids it is told to.
private final class RecordingCatalog: FFISyncCatalogWriting, @unchecked Sendable {
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

/// The metadata an opened entry carries.
private let metadataFacts = FfiAssetMetadata(
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
private func facts(
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
private func entry(
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

/// An album page's tombstone of `asset`: its delete `record`, and, as every
/// album page serves a delete, no metadata blob, no blob and no held original.
private func tombstone(_ asset: String, record: Data) -> FfiFeedEntry {
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
        changedAt: "2026-01-01T00:00:00Z"
    )
}

private let liveMetadataHash = String(repeating: "e", count: 64)
private let liveOriginalHash = String(repeating: "f", count: 64)

/// The blobs a tombstone's history names: its asset's live metadata blob and
/// the original its delete commits to, neither of which the feed lists.
private let liveSources = FfiHistorySources(
    metadataBlobHash: liveMetadataHash,
    original: FfiBlobRef(ciphertextHash: liveOriginalHash, role: "original", size: 80)
)

private func page(_ entries: [FfiFeedEntry], next: Data, more: Bool = false) -> FfiSyncPage {
    FfiSyncPage(entries: entries, nextCursor: next, hasMore: more)
}

private func stagingDirectory() -> URL {
    URL.temporaryDirectory.appending(path: "ffi-sync-\(UUID().uuidString)", directoryHint: .isDirectory)
}

private func driver(
    _ verbs: ScriptedVerbs,
    store: MemoryStore = MemoryStore(),
    catalog: RecordingCatalog = RecordingCatalog(),
    staging: URL = stagingDirectory(),
    sink: DrainSink
) -> FFISyncDriver {
    FFISyncDriver(verbs: verbs, store: store, stagingDirectory: staging, catalog: catalog) { await sink.record($0) }
}

/// The files left in `directory`, hidden ones included.
private func leftovers(in directory: URL) throws -> [URL] {
    try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
}

@Suite("FFISyncDriver — the managed lane's sync over a persisted cursor")
struct FFISyncDriverTests {
    @Test("a drain across pages hands over exactly the assets the feed newly listed, and keeps the last cursor")
    func handsOverNewlyListedAssets() async throws {
        let verbs = ScriptedVerbs(pages: [
            .success(page([entry(assetA, record: Data([1]))], next: Data([1]), more: true)),
            .success(page([entry(assetB, record: Data([2])), entry(assetA, record: Data([3]), kind: .updated)], next: Data([2]))),
        ])
        let store = MemoryStore()
        let sink = DrainSink()

        let drain = try await driver(verbs, store: store, sink: sink).sync()

        #expect(drain.newlyIndexed == [assetA, assetB])
        #expect(drain.pages == 2)
        #expect(drain.applied == 3)
        #expect(verbs.cursors == [Data(), Data([1])])
        #expect(store.state?.cursor == Data([2]))
        #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(Data([3])))
        #expect(await sink.drains == [drain])
    }

    @Test("an applied entry's head is the local chain head its asset's next entry is applied with")
    func chainsTheAppliedHead() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: Data([1])), entry(assetA, record: Data([2]), kind: .updated)], next: Data([1])))],
            chainHeads: [assetA: Data([0])]
        )

        try await driver(verbs, sink: DrainSink()).sync()

        #expect(verbs.applies.map(\.localChainHead) == [FFIPurgeDelivery.headHash(Data([0])), FFIPurgeDelivery.headHash(Data([1]))])
        #expect(verbs.applies.allSatisfy { $0.ciphertext == Data(originalHash.utf8) })
    }

    @Test("a later drain that lists nothing new hands over an empty list")
    func emptyWhenNothingIsNew() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]))], next: Data([1])))])
        let sink = DrainSink()
        let subject = driver(verbs, sink: sink)
        try await subject.sync()
        verbs.enqueue([.success(page([entry(assetA, record: Data([2]), kind: .updated)], next: Data([2])))])

        let second = try await subject.sync()

        #expect(second.newlyIndexed.isEmpty)
        #expect(second.applied == 1)
        #expect(verbs.cursors == [Data(), Data([1])])
        #expect(await sink.drains.map(\.newlyIndexed) == [[assetA], []])
    }

    @Test("an asset with a kept head is not newly listed after a relaunch")
    func keptHeadsAreNotNew() async throws {
        let kept = FFISyncState(cursor: Data([7]), heads: [assetA: FFIPurgeDelivery.headHash(Data([1]))])
        let verbs = ScriptedVerbs(pages: [
            .success(page([entry(assetA, record: Data([2]), kind: .updated), entry(assetB, record: Data([3]))], next: Data([8]))),
        ])

        let drain = try await driver(verbs, store: MemoryStore(kept), sink: DrainSink()).sync()

        #expect(verbs.cursors == [Data([7])])
        #expect(drain.newlyIndexed == [assetB])
        #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(Data([1])))
    }

    @Test("a failed page keeps the cursor and heads from before it, and hands over what the kept pages listed")
    func failedPageKeepsTheCursor() async throws {
        let verbs = ScriptedVerbs(pages: [
            .success(page([entry(assetA, record: Data([1]))], next: Data([1]), more: true)),
            .failure(FfiError.Sync(code: nil, detail: "unreachable")),
        ])
        let store = MemoryStore()
        let sink = DrainSink()
        let subject = driver(verbs, store: store, sink: sink)

        await #expect(throws: FfiError.self) { try await subject.sync() }

        #expect(store.state?.cursor == Data([1]))
        #expect(await sink.drains.map(\.newlyIndexed) == [[assetA]])

        verbs.enqueue([.success(page([entry(assetB, record: Data([2]))], next: Data([2])))])
        let retry = try await subject.sync()

        #expect(verbs.cursors.last == Data([1]))
        #expect(retry.newlyIndexed == [assetB])
    }

    @Test("a page whose apply fails partway is not kept, and lists nothing as indexed")
    func failedApplyKeepsNothingOfItsPage() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: Data([1])), entry(assetB, record: Data([2]))], next: Data([1])))],
            openings: [Data([2]): .refused(outcome: .pending(reason: "AmkNotYetLocal"))]
        )
        let store = MemoryStore()
        let sink = DrainSink()

        await #expect(throws: FFISyncError.pending(assetID: assetB, reason: "AmkNotYetLocal")) {
            try await driver(verbs, store: store, sink: sink).sync()
        }

        #expect(store.state == nil)
        #expect(store.saves == 0)
        #expect(await sink.drains == [FFISyncDrain()])
    }

    @Test("an entry level with the workspace's own chain head is neither fetched nor applied, but is newly listed")
    func ownHeadIsLevel() async throws {
        let ownRecord = Data([9])
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: ownRecord)], next: Data([1])))],
            chainHeads: [assetA: ownRecord]
        )

        let drain = try await driver(verbs, sink: DrainSink()).sync()

        #expect(drain.level == 1)
        #expect(drain.newlyIndexed == [assetA])
        #expect(verbs.fetches.isEmpty)
        #expect(verbs.applies.isEmpty)
    }

    @Test("a quarantined entry is passed and its page kept; an entry awaiting its original is not applied")
    func quarantineAndAwaitingPass() async throws {
        let entries = [entry(assetA, record: Data([1]), metadata: true), entry(assetB, record: Data([2]), metadata: true, held: false)]
        let verbs = ScriptedVerbs(
            pages: [.success(page(entries, next: Data([1])))],
            openings: [Data([1]): .refused(outcome: .quarantined(reason: "Rejected.ForgedChain", detail: nil))]
        )
        let store = MemoryStore()
        let catalog = RecordingCatalog()

        let drain = try await driver(verbs, store: store, catalog: catalog, sink: DrainSink()).sync()

        #expect(catalog.writes.isEmpty)
        #expect(drain.quarantined == 1)
        #expect(drain.awaitingOriginal == 1)
        #expect(drain.newlyIndexed == [assetA, assetB])
        #expect(verbs.opens.count == 1)
        #expect(store.state?.cursor == Data([1]))
        #expect(store.state?.heads.isEmpty == true)
    }

    @Test("an entry naming a metadata blob is opened with the blob's fetched bytes")
    func fetchesTheMetadataBlob() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))])

        try await driver(verbs, sink: DrainSink()).sync()

        #expect(verbs.fetches == [metadataHash, originalHash])
        #expect(verbs.opens.first?.metadata == Data(metadataHash.utf8))
        #expect(verbs.opens.first?.ciphertext == Data(originalHash.utf8))
        #expect(verbs.applies.isEmpty)
    }

    @Test("an applied entry with metadata hands the catalog its facts and the verified plaintext, removed after")
    func writesAnOpenedEntry() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))])
        let catalog = RecordingCatalog()
        let store = MemoryStore()
        let staging = stagingDirectory()

        let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

        #expect(drain.applied == 1)
        #expect(catalog.writes.map(\.action) == ["create"])
        #expect(catalog.writes.first?.hasMetadata == true)
        #expect(catalog.writes.first?.original == plaintextBytes)
        #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(Data([1])))
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test("a held asset's applied delete is only verified, and the catalog is handed its facts with no original")
    func writesAVerifiedLifecycleEntry() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: Data([1]), kind: .deleted)], next: Data([1])))],
            verdicts: [Data([1]): .applied(facts: facts(Data([1]), action: "delete"))],
            chainHeads: [assetA: Data([0])]
        )
        let catalog = RecordingCatalog()

        let drain = try await driver(verbs, catalog: catalog, sink: DrainSink()).sync()

        #expect(drain.applied == 1)
        #expect(verbs.opens.isEmpty)
        #expect(verbs.applies.count == 1)
        #expect(catalog.writes == [CatalogCall(action: "delete", hasMetadata: false, original: nil, originalPath: nil)])
    }

    @Test("an opened entry the chokepoint refuses is passed as its verdict and writes nothing")
    func refusedOpenWritesNothing() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))],
            openings: [Data([1]): .refused(outcome: .quarantined(reason: "Rejected.ForgedChain", detail: nil))]
        )
        let catalog = RecordingCatalog()
        let staging = stagingDirectory()

        let drain = try await driver(verbs, catalog: catalog, staging: staging, sink: DrainSink()).sync()

        #expect(drain.quarantined == 1)
        #expect(drain.applied == 0)
        #expect(catalog.writes.isEmpty)
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test(
        "an applied entry whose original does not open is quarantined and writes nothing",
        arguments: [
            FfiOpenedOriginal.undecryptable(facts: facts(Data([1]), metadata: metadataFacts)),
            FfiOpenedOriginal.digestMismatch(facts: facts(Data([1]), metadata: metadataFacts)),
        ]
    )
    func unopenedOriginalWritesNothing(opened: FfiOpenedOriginal) async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))],
            openings: [Data([1]): opened]
        )
        let catalog = RecordingCatalog()
        let store = MemoryStore()

        let drain = try await driver(verbs, store: store, catalog: catalog, sink: DrainSink()).sync()

        #expect(drain.quarantined == 1)
        #expect(catalog.writes.isEmpty)
        #expect(store.state?.heads.isEmpty == true)
        #expect(store.state?.cursor == Data([1]))
    }

    @Test("a catalog write that fails keeps nothing of its page, and removes the staged plaintext")
    func failedWriteKeepsNothingOfItsPage() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))])
        let catalog = RecordingCatalog(failing: [assetA])
        let store = MemoryStore()
        let staging = stagingDirectory()

        await #expect(throws: FfiError.self) {
            try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()
        }

        #expect(catalog.writes.count == 1)
        #expect(store.saves == 0)
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test("an unheld asset's create is opened on its own: its history is empty")
    func unheldCreateOpensWithoutHistory() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))])

        try await driver(verbs, sink: DrainSink()).sync()

        #expect(verbs.historyFetches == [Data([1])])
        #expect(verbs.historyOpens.isEmpty)
        #expect(verbs.opens.map(\.manifest) == [Data([1])])
    }

    @Test("an unheld asset first seen past its create is opened against its fetched history and adopted")
    func adoptsFromHistory() async throws {
        let record = Data([3])
        let history = [Data([1]), Data([2])]
        let kept = FFISyncState(cursor: Data([7]), heads: [assetA: FFIPurgeDelivery.headHash(Data([2]))])
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([8])))],
            histories: [record: .success(history)]
        )
        let catalog = RecordingCatalog()
        let store = MemoryStore(kept)
        let staging = stagingDirectory()

        let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

        #expect(drain.applied == 1)
        #expect(verbs.historyFetches == [record])
        #expect(verbs.opens.isEmpty)
        #expect(verbs.applies.isEmpty)
        #expect(verbs.historyOpens == [HistoryOpenCall(
            call: ApplyCall(
                manifest: record,
                metadata: Data(metadataHash.utf8),
                localChainHead: FFIPurgeDelivery.headHash(Data([2])),
                ciphertext: Data(originalHash.utf8)
            ),
            history: history
        )])
        #expect(verbs.fetches == [metadataHash, originalHash])
        #expect(catalog.writes.map(\.action) == ["metadata-update"])
        #expect(catalog.writes.first?.original == plaintextBytes)
        #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(record))
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test("an asset adopted from its history is held for the rest of the drain: its next entry reads no history")
    func adoptedAssetIsHeldAfter() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page(
                [
                    entry(assetA, record: Data([3]), kind: .updated, metadata: true),
                    entry(assetA, record: Data([4]), kind: .updated, metadata: true),
                ],
                next: Data([1])
            ))],
            histories: [Data([3]): .success([Data([1]), Data([2])])]
        )

        let drain = try await driver(verbs, sink: DrainSink()).sync()

        #expect(drain.applied == 2)
        #expect(verbs.historyFetches == [Data([3])])
        #expect(verbs.historyOpens.map(\.call.manifest) == [Data([3])])
        #expect(verbs.opens.map(\.manifest) == [Data([4])])
        #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(Data([3])))
    }

    @Test("an asset its create adopted is held for the rest of the drain: its next entry reads no history")
    func adoptedCreateIsHeldAfter() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page(
                [
                    entry(assetA, record: Data([1]), metadata: true),
                    entry(assetA, record: Data([2]), kind: .updated, metadata: true),
                ],
                next: Data([1])
            ))],
            openings: [Data([1]): .opened(facts: facts(Data([1]), metadata: metadataFacts, adopted: true))]
        )

        let drain = try await driver(verbs, sink: DrainSink()).sync()

        #expect(drain.applied == 2)
        #expect(verbs.historyFetches == [Data([1])])
        #expect(verbs.historyOpens.isEmpty)
        #expect(verbs.opens.map(\.manifest) == [Data([1]), Data([2])])
        #expect(verbs.opens.last?.localChainHead == FFIPurgeDelivery.headHash(Data([1])))
    }

    @Test("a held asset's entry is opened against its own chain, reading no history")
    func heldAssetReadsNoHistory() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: Data([2]), kind: .updated, metadata: true)], next: Data([1])))],
            chainHeads: [assetA: Data([1])]
        )

        try await driver(verbs, sink: DrainSink()).sync()

        #expect(verbs.historyFetches.isEmpty)
        #expect(verbs.historyOpens.isEmpty)
        #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(Data([1])))
    }

    @Test("an unheld asset first seen at its delete on an album page is adopted from the blobs its history names, and its restore opens against the adopted head")
    func albumTombstoneIsAdoptedFromItsRecords() async throws {
        let delete = Data([3])
        let restore = Data([4])
        let history = [Data([1]), Data([2])]
        let verbs = ScriptedVerbs(
            pages: [.success(page(
                [
                    tombstone(assetA, record: delete),
                    entry(assetA, record: restore, kind: .updated, metadata: true),
                ],
                next: Data([1])
            ))],
            openings: [restore: .opened(facts: facts(restore, action: "trash-restore", metadata: metadataFacts, adopted: true))],
            histories: [delete: .success(history)],
            historyOpenings: [delete: .opened(facts: facts(delete, action: "delete", metadata: metadataFacts, adopted: true))],
            sources: [delete: .success(liveSources)]
        )
        let catalog = RecordingCatalog()
        let store = MemoryStore()
        let staging = stagingDirectory()

        let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

        #expect(drain.applied == 2)
        #expect(drain.awaitingOriginal == 0)
        #expect(verbs.historyFetches == [delete])
        #expect(verbs.sourceNamings == [SourcesCall(manifest: delete, history: history)])
        #expect(verbs.fetches == [liveMetadataHash, liveOriginalHash, metadataHash, originalHash])
        #expect(verbs.fetchedSizes == [liveSources.original.size, 64])
        #expect(verbs.historyOpens == [HistoryOpenCall(
            call: ApplyCall(
                manifest: delete,
                metadata: Data(liveMetadataHash.utf8),
                localChainHead: nil,
                ciphertext: Data(liveOriginalHash.utf8)
            ),
            history: history
        )])
        #expect(verbs.opens.map(\.manifest) == [restore])
        #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(delete))
        #expect(verbs.applies.isEmpty)
        #expect(catalog.writes.map(\.action) == ["delete", "trash-restore"])
        #expect(catalog.writes.first?.original == plaintextBytes)
        #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(restore))
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test("an album page's tombstone whose history names no live metadata blob is quarantined, fetching no blob")
    func unboundTombstoneIsRefused() async throws {
        let delete = Data([3])
        let history = [Data([1]), Data([2])]
        let verbs = ScriptedVerbs(
            pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
            histories: [delete: .success(history)],
            sources: [delete: .failure(FfiError.Sync(code: nil, detail: "no record of the chain binds a metadata blob"))]
        )
        let catalog = RecordingCatalog()
        let store = MemoryStore()
        let staging = stagingDirectory()

        let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

        #expect(drain.quarantined == 1)
        #expect(drain.applied == 0)
        #expect(verbs.sourceNamings == [SourcesCall(manifest: delete, history: history)])
        #expect(verbs.fetches.isEmpty)
        #expect(verbs.historyOpens.isEmpty)
        #expect(verbs.opens.isEmpty)
        #expect(verbs.applies.isEmpty)
        #expect(catalog.writes.isEmpty)
        #expect(store.state?.cursor == Data([1]))
        #expect(store.state?.heads.isEmpty == true)
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test("an album page's tombstone whose records do not decode is quarantined before its sources are named")
    func unreadableTombstoneHistoryQuarantines() async throws {
        let delete = Data([3])
        let verbs = ScriptedVerbs(
            pages: [.success(page([tombstone(assetA, record: delete)], next: Data([1])))],
            histories: [delete: .failure(FfiError.Sync(code: nil, detail: "a record does not decode"))]
        )

        let drain = try await driver(verbs, sink: DrainSink()).sync()

        #expect(drain.quarantined == 1)
        #expect(verbs.sourceNamings.isEmpty)
        #expect(verbs.fetches.isEmpty)
    }

    @Test("a held asset's album-page tombstone reads no history: it awaits its original")
    func heldTombstoneReadsNoHistory() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([tombstone(assetA, record: Data([2]))], next: Data([1])))],
            chainHeads: [assetA: Data([1])]
        )

        let drain = try await driver(verbs, sink: DrainSink()).sync()

        #expect(drain.awaitingOriginal == 1)
        #expect(verbs.historyFetches.isEmpty)
        #expect(verbs.sourceNamings.isEmpty)
        #expect(verbs.fetches.isEmpty)
    }

    @Test("an unheld asset first seen at its delete in trash is adopted from its history with its live metadata blob, and its restore opens against the adopted head")
    func trashedDeleteIsAdoptedAndRestored() async throws {
        let delete = Data([3])
        let restore = Data([4])
        let history = [Data([1]), Data([2])]
        let verbs = ScriptedVerbs(
            pages: [.success(page(
                [
                    entry(assetA, record: delete, kind: .deleted, metadata: true),
                    entry(assetA, record: restore, kind: .updated, metadata: true),
                ],
                next: Data([1])
            ))],
            openings: [restore: .opened(facts: facts(restore, action: "trash-restore", metadata: metadataFacts, adopted: true))],
            histories: [delete: .success(history)],
            historyOpenings: [delete: .opened(facts: facts(delete, action: "delete", metadata: metadataFacts, adopted: true))]
        )
        let catalog = RecordingCatalog()
        let store = MemoryStore()
        let staging = stagingDirectory()

        let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

        #expect(drain.applied == 2)
        #expect(drain.skipped == 0)
        #expect(verbs.fetches == [metadataHash, originalHash, metadataHash, originalHash])
        #expect(verbs.historyFetches == [delete])
        #expect(verbs.historyOpens == [HistoryOpenCall(
            call: ApplyCall(
                manifest: delete,
                metadata: Data(metadataHash.utf8),
                localChainHead: nil,
                ciphertext: Data(originalHash.utf8)
            ),
            history: history
        )])
        #expect(verbs.opens.map(\.manifest) == [restore])
        #expect(verbs.opens.first?.metadata == Data(metadataHash.utf8))
        #expect(verbs.opens.first?.localChainHead == FFIPurgeDelivery.headHash(delete))
        #expect(verbs.applies.isEmpty)
        #expect(catalog.writes.map(\.action) == ["delete", "trash-restore"])
        #expect(store.state?.heads[assetA] == FFIPurgeDelivery.headHash(restore))
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test("an entry whose metadata blob address is not a content address is skipped, fetching nothing")
    func malformedMetadataAddressIsSkipped() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page(
                [entry(assetA, record: Data([1]), metadata: true, metadataAddress: Data("../../etc/passwd".utf8))],
                next: Data([1])
            ))]
        )
        let store = MemoryStore()

        let drain = try await driver(verbs, store: store, sink: DrainSink()).sync()

        #expect(drain.skipped == 1)
        #expect(drain.applied == 0)
        #expect(verbs.fetches.isEmpty)
        #expect(verbs.historyFetches.isEmpty)
        #expect(verbs.opens.isEmpty)
        #expect(store.state?.cursor == Data([1]))
    }

    @Test("a metadata blob that cannot be fetched stops the drain before its page is kept, staging nothing")
    func failedMetadataFetchKeepsNothing() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: Data([1]), metadata: true)], next: Data([1])))],
            metadataFailure: FfiError.Fetch(code: nil, detail: "blob exceeds the limit for a blob of unlisted size")
        )
        let store = MemoryStore()
        let staging = stagingDirectory()

        await #expect(throws: FfiError.self) {
            try await driver(verbs, store: store, staging: staging, sink: DrainSink()).sync()
        }

        #expect(store.saves == 0)
        #expect(verbs.fetches == [metadataHash])
        #expect(verbs.opens.isEmpty)
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test("a history the chokepoint refuses is passed as its verdict and writes nothing")
    func refusedHistoryWritesNothing() async throws {
        let record = Data([3])
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([1])))],
            histories: [record: .success([Data([1]), Data([2])])],
            historyOpenings: [record: .refused(outcome: .quarantined(reason: "Rejected.ForgedChain", detail: nil))]
        )
        let catalog = RecordingCatalog()
        let store = MemoryStore()
        let staging = stagingDirectory()

        let drain = try await driver(verbs, store: store, catalog: catalog, staging: staging, sink: DrainSink()).sync()

        #expect(drain.quarantined == 1)
        #expect(drain.applied == 0)
        #expect(catalog.writes.isEmpty)
        #expect(store.state?.cursor == Data([1]))
        #expect(store.state?.heads.isEmpty == true)
        #expect(try leftovers(in: staging).isEmpty)
    }

    @Test("a history whose records do not decode quarantines its entry before any blob is fetched")
    func unreadableHistoryQuarantines() async throws {
        let record = Data([3])
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([1])))],
            histories: [record: .failure(FfiError.Sync(code: nil, detail: "a record does not decode"))]
        )
        let store = MemoryStore()
        let catalog = RecordingCatalog()

        let drain = try await driver(verbs, store: store, catalog: catalog, sink: DrainSink()).sync()

        #expect(drain.quarantined == 1)
        #expect(verbs.fetches.isEmpty)
        #expect(verbs.historyOpens.isEmpty)
        #expect(verbs.opens.isEmpty)
        #expect(catalog.writes.isEmpty)
        #expect(store.state?.cursor == Data([1]))
    }

    @Test("a record that cannot be fetched stops the drain before its page is kept, and the next drain reads it again")
    func failedRecordFetchKeepsNothing() async throws {
        let record = Data([3])
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([1])))],
            histories: [record: .failure(FfiError.Fetch(code: nil, detail: "provenance record: unreachable"))]
        )
        let store = MemoryStore()
        let staging = stagingDirectory()
        let subject = driver(verbs, store: store, staging: staging, sink: DrainSink())

        await #expect(throws: FfiError.self) { try await subject.sync() }

        #expect(store.saves == 0)
        #expect(verbs.fetches.isEmpty)
        #expect(try leftovers(in: staging).isEmpty)

        verbs.enqueue([.success(page([entry(assetA, record: record, kind: .updated, metadata: true)], next: Data([1])))])
        await #expect(throws: FfiError.self) { try await subject.sync() }

        #expect(verbs.cursors == [Data(), Data()])
        #expect(verbs.historyFetches == [record, record])
    }

    @Test("a stale cursor restarts the drain from the feed's start once, keeping the heads")
    func staleCursorResyncs() async throws {
        let kept = FFISyncState(cursor: Data([7]), heads: [assetA: FFIPurgeDelivery.headHash(Data([1]))])
        let verbs = ScriptedVerbs(pages: [
            .failure(FfiError.Sync(code: FFISyncDriver.staleCursorCode, detail: "new epoch")),
            .success(page([entry(assetA, record: Data([1])), entry(assetC, record: Data([2]))], next: Data([1]))),
        ])

        let drain = try await driver(verbs, store: MemoryStore(kept), sink: DrainSink()).sync()

        #expect(drain.resynced)
        #expect(verbs.cursors == [Data([7]), Data()])
        #expect(drain.level == 1)
        #expect(drain.newlyIndexed == [assetC])
    }

    @Test("a page that claims more without advancing the cursor stops the drain")
    func stalledCursorStops() async throws {
        let verbs = ScriptedVerbs(pages: [.success(page([], next: Data(), more: true))])

        await #expect(throws: FeedDrainError.cursorStalled) {
            try await driver(verbs, sink: DrainSink()).sync()
        }
    }

    @Test("the state file round-trips and an absent file loads as none")
    func stateFileRoundTrips() throws {
        let file = FFISyncStateFile(url: stagingDirectory().appending(path: "state.json"))
        #expect(try file.load() == nil)
        let state = FFISyncState(cursor: Data([1, 2]), heads: [assetA: Data([3])])

        try file.save(state)

        #expect(try file.load() == state)
    }

    @Test("the blob root is the API origin's /v1 root")
    func blobRoot() {
        #expect(FFISessionSyncVerbs.blobBaseURL(apiBaseURL: "https://capsule.test:8443") == "https://capsule.test:8443/v1")
        #expect(FFISessionSyncVerbs.blobBaseURL(apiBaseURL: "https://capsule.test/") == "https://capsule.test/v1")
    }
}
