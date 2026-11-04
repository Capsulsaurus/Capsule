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
/// blob as its hash's bytes, answers each apply or open with the verdict
/// scripted for the manifest, and records every call. An open that answers
/// `opened` writes ``plaintextBytes`` at its destination, as the workspace does.
private final class ScriptedVerbs: FFISyncDriverVerbs, @unchecked Sendable {
    private let lock = NSLock()
    private var pages: [Result<FfiSyncPage, any Error>]
    private var pulledFrom: [Data] = []
    private var fetched: [String] = []
    private var applyCalls: [ApplyCall] = []
    private var openCalls: [ApplyCall] = []
    private let verdicts: [Data: FfiSyncApplyOutcome]
    private let openings: [Data: FfiOpenedOriginal]
    private let chainHeads: [String: Data]

    init(
        pages: [Result<FfiSyncPage, any Error>],
        verdicts: [Data: FfiSyncApplyOutcome] = [:],
        openings: [Data: FfiOpenedOriginal] = [:],
        chainHeads: [String: Data] = [:]
    ) {
        self.pages = pages
        self.verdicts = verdicts
        self.openings = openings
        self.chainHeads = chainHeads
    }

    var opens: [ApplyCall] {
        lock.withLock { openCalls }
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

    func fetchBlob(hash: String, size _: UInt64, to path: URL) throws {
        lock.withLock { fetched.append(hash) }
        try Data(hash.utf8).write(to: path)
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
            return openings[head.manifestCbor] ?? .opened(facts: facts(head.manifestCbor, metadata: metadataFacts))
        }
        if case .opened = opened {
            try plaintextBytes.write(to: destination)
        }
        return opened
    }

    func provenanceHead(assetId: String) throws -> Data {
        guard let head = chainHeads[assetId] else {
            throw FfiError.Workspace(detail: "asset \(assetId) is not managed by this workspace")
        }
        return head
    }

    func assetIds() -> [String] {
        Array(chainHeads.keys)
    }
}

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
private func facts(_ manifest: Data, action: String = "create", metadata: FfiAssetMetadata? = nil) -> FfiAssetFacts {
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
        metadata: metadata
    )
}

/// A feed entry listing `asset` with `record` as its latest record, carrying
/// an original the server holds unless `held` is false.
private func entry(
    _ asset: String,
    record: Data,
    kind: FfiChangeKind = .created,
    metadata: Bool = false,
    held: Bool = true
) -> FfiFeedEntry {
    FfiFeedEntry(
        albumId: Data(repeating: 0xAB, count: 16),
        syncSeq: 1,
        protocolVersion: "2026-01-01",
        kind: kind,
        assetId: Data(asset.utf8),
        manifestCbor: record,
        metadataBlob: metadata ? Data(metadataHash.utf8) : Data(),
        blobs: FfiBlobManifest(
            original: FfiBlobRef(ciphertextHash: originalHash, role: "original", size: 64),
            derivatives: metadata ? [FfiBlobRef(ciphertextHash: metadataHash, role: "metadata", size: 64)] : []
        ),
        originalHeld: held,
        changedAt: "2026-01-01T00:00:00Z"
    )
}

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
        let verbs = ScriptedVerbs(pages: [
            .success(page([entry(assetA, record: Data([1])), entry(assetA, record: Data([2]), kind: .updated)], next: Data([1]))),
        ])

        try await driver(verbs, sink: DrainSink()).sync()

        #expect(verbs.applies.map(\.localChainHead) == [nil, FFIPurgeDelivery.headHash(Data([1]))])
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
        #expect(verbs.applies.first?.localChainHead == FFIPurgeDelivery.headHash(Data([1])))
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
            verdicts: [Data([2]): .pending(reason: "AmkNotYetLocal")]
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
        let entries = [entry(assetA, record: Data([1])), entry(assetB, record: Data([2]), held: false)]
        let verbs = ScriptedVerbs(
            pages: [.success(page(entries, next: Data([1])))],
            verdicts: [Data([1]): .quarantined(reason: "Rejected.ForgedChain", detail: nil)]
        )
        let store = MemoryStore()
        let catalog = RecordingCatalog()

        let drain = try await driver(verbs, store: store, catalog: catalog, sink: DrainSink()).sync()

        #expect(catalog.writes.isEmpty)
        #expect(drain.quarantined == 1)
        #expect(drain.awaitingOriginal == 1)
        #expect(drain.newlyIndexed == [assetA, assetB])
        #expect(verbs.applies.count == 1)
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

    @Test("an applied delete is only verified, and the catalog is handed its facts with no original")
    func writesAVerifiedLifecycleEntry() async throws {
        let verbs = ScriptedVerbs(
            pages: [.success(page([entry(assetA, record: Data([1]), kind: .deleted)], next: Data([1])))],
            verdicts: [Data([1]): .applied(facts: facts(Data([1]), action: "delete"))]
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
