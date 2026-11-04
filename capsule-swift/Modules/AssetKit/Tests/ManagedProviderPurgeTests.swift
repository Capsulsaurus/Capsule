import Foundation
import Testing

import AssetKit
import CapsuleCatalog
import CapsuleFoundation
import CapsuleTestSupport
import ManagedStore

/// Permanent deletion is the one operation whose *ordering* is a correctness
/// property rather than a preference, so it gets its own suite.
@Suite("ManagedProvider.purge")
struct ManagedProviderPurgeTests {
    private let layout = ManagedLibraryLayout(root: URL(filePath: "/capsule/Library"))
    // 1_720_000_000 → 2024-07-03 UTC, so the partition is media/2024/2024-07.
    private static let captureTimestamp: Int64 = 1720000000
    private var captureDate: Date { Date(timeIntervalSince1970: TimeInterval(Self.captureTimestamp)) }

    /// A row already in the trash: a purge never skips it.
    private func makeAsset(id: String) -> CatalogAsset {
        var asset = CatalogAsset(
            id: id,
            assetType: "photo",
            captureTimestamp: Self.captureTimestamp,
            importTimestamp: Self.captureTimestamp,
            hashSHA256: String(repeating: "a", count: 64)
        )
        asset.isDeleted = true
        asset.deletedAt = Self.captureTimestamp
        return asset
    }

    private func makeProvider(
        catalog: MockCatalog,
        fileStore: MockFileStore,
        purgeDelivery: (any PurgeDelivering)? = nil
    ) -> ManagedProvider {
        ManagedProvider(
            library: ManagedLibrary(layout: layout, fileStore: fileStore, catalog: catalog),
            authGate: MockLocalAuthGate(),
            purgeDelivery: purgeDelivery
        )
    }

    @Test("purging takes the bytes as well as the row")
    func purgeRemovesBytesAndRow() async throws {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let media = layout.mediaFile(uuid: "asset-1", fileExtension: "heic", captureDate: captureDate)
        let sidecar = layout.sidecarFile(uuid: "asset-1", captureDate: captureDate)
        try await catalog.insertAsset(makeAsset(id: "asset-1"))
        try await fileStore.write(Data("bytes".utf8), to: media)
        try await fileStore.write(Data("sidecar".utf8), to: sidecar)

        try await makeProvider(catalog: catalog, fileStore: fileStore).purge(.managed(uuid: "asset-1"))

        #expect(try await catalog.asset(id: "asset-1") == nil)
        #expect(await fileStore.fileExists(at: media) == false)
        #expect(await fileStore.fileExists(at: sidecar) == false)
    }

    @Test("a file that cannot be removed leaves the row behind, and throws")
    func failedFileRemovalKeepsTheRow() async throws {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let media = layout.mediaFile(uuid: "asset-1", fileExtension: "heic", captureDate: captureDate)
        try await catalog.insertAsset(makeAsset(id: "asset-1"))
        try await fileStore.write(Data("bytes".utf8), to: media)
        await fileStore.injectFailure(on: .remove)

        let provider = makeProvider(catalog: catalog, fileStore: fileStore)
        await #expect(throws: (any Error).self) {
            try await provider.purge(.managed(uuid: "asset-1"))
        }

        // The contract: reporting a photograph destroyed while its bytes are still
        // on disk is the failure worth designing against. Keeping the row means the
        // asset stays in Recently Deleted and the purge can be retried.
        #expect(try await catalog.asset(id: "asset-1") != nil)
        #expect(await fileStore.fileExists(at: media))
    }

    @Test("purging an asset whose row is already gone is not an error")
    func purgeOfAMissingRowSucceeds() async throws {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        try await makeProvider(catalog: catalog, fileStore: fileStore).purge(.managed(uuid: "never-existed"))
        #expect(await fileStore.fileCount == 0)
    }

    @Test("a non-managed identifier is ignored rather than mistaken for one")
    func ignoresForeignIdentifiers() async throws {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        try await catalog.insertAsset(makeAsset(id: "asset-1"))

        try await makeProvider(catalog: catalog, fileStore: fileStore)
            .purge(.photoKit(localIdentifier: "PK/L0/001"))

        #expect(try await catalog.asset(id: "asset-1") != nil)
    }

    // MARK: Delivery to the server

    /// A provider over trashed rows `ids`, delivering through `delivery`.
    private func makeDeliveringProvider(
        trashed ids: [String],
        delivery: ScriptedPurgeDelivery
    ) async throws -> ManagedProvider {
        let catalog = MockCatalog()
        for id in ids {
            try await catalog.insertAsset(makeAsset(id: id))
        }
        return makeProvider(catalog: catalog, fileStore: MockFileStore(), purgeDelivery: delivery)
    }

    @Test("a purge produces exactly one delivery of its asset")
    func purgeDeliversOnce() async throws {
        let delivery = ScriptedPurgeDelivery(outcomes: ["asset-1": [.delivered]])
        let provider = try await makeDeliveringProvider(trashed: ["asset-1"], delivery: delivery)

        try await provider.purge(.managed(uuid: "asset-1"))

        #expect(await delivery.delivered == ["asset-1"])
    }

    @Test("a purge that did not happen delivers nothing: a file left behind, an asset already gone, a foreign id")
    func noPurgeNoDelivery() async throws {
        let delivery = ScriptedPurgeDelivery()
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let media = layout.mediaFile(uuid: "asset-1", fileExtension: "heic", captureDate: captureDate)
        try await catalog.insertAsset(makeAsset(id: "asset-1"))
        try await fileStore.write(Data("bytes".utf8), to: media)
        await fileStore.injectFailure(on: .remove)
        let provider = makeProvider(catalog: catalog, fileStore: fileStore, purgeDelivery: delivery)

        await #expect(throws: (any Error).self) { try await provider.purge(.managed(uuid: "asset-1")) }
        try await provider.purge(.managed(uuid: "never-existed"))
        try await provider.purge(.photoKit(localIdentifier: "PK/L0/001"))

        #expect(await delivery.delivered.isEmpty)
    }

    @Test("a delivery that cannot be attempted leaves the purge standing")
    func throwingDeliveryKeepsThePurge() async throws {
        let delivery = ScriptedPurgeDelivery(deliveryFailure: ScriptedFailure())
        let catalog = MockCatalog()
        try await catalog.insertAsset(makeAsset(id: "asset-1"))
        let provider = makeProvider(catalog: catalog, fileStore: MockFileStore(), purgeDelivery: delivery)

        try await provider.purge(.managed(uuid: "asset-1"))

        #expect(try await catalog.asset(id: "asset-1") == nil)
        #expect(await delivery.delivered == ["asset-1"])
    }

    @Test("a held or failed delivery goes out again on the next redelivery")
    func heldAndFailedAreRedelivered() async throws {
        let delivery = ScriptedPurgeDelivery(
            outcomes: ["asset-1": [.held, .delivered], "asset-2": [.failed(permanent: false), .delivered]],
            purged: ["asset-1", "asset-2"]
        )
        let provider = try await makeDeliveringProvider(trashed: ["asset-1", "asset-2"], delivery: delivery)

        try await provider.purge(.managed(uuid: "asset-1"))
        try await provider.purge(.managed(uuid: "asset-2"))
        await provider.redeliverPurges()

        #expect(await delivery.delivered == ["asset-1", "asset-2", "asset-1", "asset-2"])
    }

    @Test("a purge the server refused for good is skipped on redelivery; a transient failure is not")
    func permanentRefusalIsSkipped() async throws {
        let delivery = ScriptedPurgeDelivery(
            outcomes: [
                "asset-1": [.failed(permanent: true)],
                "asset-2": [.failed(permanent: false), .failed(permanent: false), .failed(permanent: false)],
            ],
            purged: ["asset-1", "asset-2"]
        )
        let provider = try await makeDeliveringProvider(trashed: ["asset-1", "asset-2"], delivery: delivery)

        try await provider.purge(.managed(uuid: "asset-1"))
        try await provider.purge(.managed(uuid: "asset-2"))
        await provider.redeliverPurges()
        await provider.redeliverPurges()

        #expect(await delivery.delivered == ["asset-1", "asset-2", "asset-2", "asset-2"])
    }

    @Test("a redelivery walks every kept purge, also those from before this provider started")
    func redeliveryWalksKeptPurges() async throws {
        let delivery = ScriptedPurgeDelivery(purged: ["kept-1", "kept-2"])
        let provider = try await makeDeliveringProvider(trashed: [], delivery: delivery)

        await provider.redeliverPurges()

        #expect(await delivery.delivered == ["kept-1", "kept-2"])
    }

    @Test("a redelivery whose purge list cannot be read sends nothing; with no port there is nothing to send")
    func unreadableListSendsNothing() async throws {
        let delivery = ScriptedPurgeDelivery(purged: ["kept-1"], listFailure: ScriptedFailure())
        let provider = try await makeDeliveringProvider(trashed: [], delivery: delivery)

        await provider.redeliverPurges()
        await makeProvider(catalog: MockCatalog(), fileStore: MockFileStore()).redeliverPurges()

        #expect(await delivery.delivered.isEmpty)
    }
}

/// A failure a scripted port throws.
private struct ScriptedFailure: Error {}

/// A scripted ``PurgeDelivering``: answers each delivery of an asset with the
/// next outcome scripted for it (``PurgeDelivery/held`` once they run out),
/// lists `purged` as the workspace's kept purges, and records every delivery.
private actor ScriptedPurgeDelivery: PurgeDelivering {
    private(set) var delivered: [String] = []
    private var outcomes: [String: [PurgeDelivery]]
    private let purged: [String]
    private let listFailure: (any Error)?
    private let deliveryFailure: (any Error)?

    init(
        outcomes: [String: [PurgeDelivery]] = [:],
        purged: [String] = [],
        listFailure: (any Error)? = nil,
        deliveryFailure: (any Error)? = nil
    ) {
        self.outcomes = outcomes
        self.purged = purged
        self.listFailure = listFailure
        self.deliveryFailure = deliveryFailure
    }

    func deliverPurge(assetID: String) throws -> PurgeDelivery {
        delivered.append(assetID)
        if let deliveryFailure { throw deliveryFailure }
        guard var queue = outcomes[assetID], !queue.isEmpty else { return .held }
        let next = queue.removeFirst()
        outcomes[assetID] = queue
        return next
    }

    func purgedAssetIDs() throws -> [String] {
        if let listFailure { throw listFailure }
        return purged
    }
}

/// What a workspace and a lifecycle delivery saw, in the order they saw it.
private actor Journal {
    private(set) var entries: [String] = []

    func record(_ entry: String) {
        entries.append(entry)
    }
}

/// A workspace that journals each trash verb; `failing` names verbs it
/// refuses after journaling them.
private struct JournalingWorkspace: ManagedWorkspace {
    let journal: Journal
    var failing: Set<String> = []

    func sealAsset(fileName _: String, bytes _: Data) -> String {
        "sealed"
    }

    func deleteAsset(id: String, retainDays _: UInt32) async throws {
        await journal.record("delete \(id)")
        if failing.contains("delete \(id)") { throw ScriptedFailure() }
    }

    func restoreAsset(id: String) async throws {
        await journal.record("restore \(id)")
        if failing.contains("restore \(id)") { throw TrashRefusal.retentionElapsed(retentionUntil: nil) }
    }

    func purgeAsset(id: String) async -> ManagedPurgeOutcome {
        await journal.record("purge \(id)")
        return ManagedPurgeOutcome(assetID: id)
    }
}

/// A scripted ``LifecycleDelivering``: journals each delivery, answers it
/// with the next outcome scripted for its asset (``LifecycleDelivery/held``
/// once they run out), and lists `held` as the workspace's assets.
private actor ScriptedLifecycleDelivery: LifecycleDelivering {
    private let journal: Journal
    private var outcomes: [String: [LifecycleDelivery]]
    private let held: [String]
    private let listFailure: (any Error)?
    private let deliveryFailure: (any Error)?
    private(set) var delivered: [String] = []

    init(
        journal: Journal = Journal(),
        outcomes: [String: [LifecycleDelivery]] = [:],
        held: [String] = [],
        listFailure: (any Error)? = nil,
        deliveryFailure: (any Error)? = nil
    ) {
        self.journal = journal
        self.outcomes = outcomes
        self.held = held
        self.listFailure = listFailure
        self.deliveryFailure = deliveryFailure
    }

    func deliverLifecycle(assetID: String) async throws -> LifecycleDelivery {
        delivered.append(assetID)
        await journal.record("deliver \(assetID)")
        if let deliveryFailure { throw deliveryFailure }
        guard var queue = outcomes[assetID], !queue.isEmpty else { return .held }
        let next = queue.removeFirst()
        outcomes[assetID] = queue
        return next
    }

    func heldAssetIDs() throws -> [String] {
        if let listFailure { throw listFailure }
        return held
    }
}

/// A delete's or restore's signed record goes to the server after the
/// workspace holds it, and a relaunch walks the ones still undelivered.
@Suite("ManagedProvider delivers held delete and restore records")
struct ManagedProviderLifecycleDeliveryTests {
    private func makeProvider(
        workspace: JournalingWorkspace,
        delivery: ScriptedLifecycleDelivery?
    ) -> ManagedProvider {
        ManagedProvider(
            library: ManagedLibrary(
                layout: ManagedLibraryLayout(root: URL(filePath: "/capsule/Library")),
                fileStore: MockFileStore(),
                catalog: MockCatalog(),
                workspace: workspace
            ),
            authGate: MockLocalAuthGate(),
            lifecycleDelivery: delivery
        )
    }

    @Test("each managed delete is delivered once, after the workspace holds every delete of the batch")
    func deleteDeliversAfterTheWorkspace() async throws {
        let journal = Journal()
        let delivery = ScriptedLifecycleDelivery(journal: journal)
        let provider = makeProvider(workspace: JournalingWorkspace(journal: journal), delivery: delivery)

        try await provider.delete([
            .managed(uuid: "a"),
            .photoKit(localIdentifier: "PK/L0/001"),
            .managed(uuid: "b"),
        ])

        #expect(await journal.entries == ["delete a", "delete b", "deliver a", "deliver b"])
    }

    @Test("a restore is delivered once after the workspace holds it; a refused one delivers nothing")
    func restoreDeliversAfterTheWorkspace() async throws {
        let journal = Journal()
        let delivery = ScriptedLifecycleDelivery(journal: journal)
        let provider = makeProvider(
            workspace: JournalingWorkspace(journal: journal, failing: ["restore b"]),
            delivery: delivery
        )

        try await provider.restore(.managed(uuid: "a"))
        await #expect(throws: TrashRefusal.self) { try await provider.restore(.managed(uuid: "b")) }
        try await provider.restore(.photoKit(localIdentifier: "PK/L0/001"))

        #expect(await journal.entries == ["restore a", "deliver a", "restore b"])
    }

    @Test("a delete the workspace refuses stops the batch before anything is delivered")
    func refusedDeleteDeliversNothing() async {
        let journal = Journal()
        let delivery = ScriptedLifecycleDelivery(journal: journal)
        let provider = makeProvider(
            workspace: JournalingWorkspace(journal: journal, failing: ["delete b"]),
            delivery: delivery
        )

        await #expect(throws: ScriptedFailure.self) {
            try await provider.delete([.managed(uuid: "a"), .managed(uuid: "b")])
        }

        #expect(await delivery.delivered.isEmpty)
    }

    @Test("a delivery that cannot be attempted leaves the delete and restore standing")
    func throwingDeliveryIsSwallowed() async throws {
        let journal = Journal()
        let delivery = ScriptedLifecycleDelivery(journal: journal, deliveryFailure: ScriptedFailure())
        let provider = makeProvider(workspace: JournalingWorkspace(journal: journal), delivery: delivery)

        try await provider.delete([.managed(uuid: "a")])
        try await provider.restore(.managed(uuid: "a"))

        #expect(await delivery.delivered == ["a", "a"])
    }

    @Test("a redelivery walks every held asset, retrying held and transient ones and skipping those settled for good")
    func redeliverySkipsSettledAssets() async throws {
        let delivery = ScriptedLifecycleDelivery(
            outcomes: [
                "held": [.held, .held, .held],
                "transient": [.failed(permanent: false), .failed(permanent: false), .delivered],
                "refused": [.failed(permanent: true)],
                "too-late": [.withheld(retentionUntil: "2026-01-02T00:00:00Z")],
            ],
            held: ["held", "transient", "refused", "too-late"]
        )
        let provider = makeProvider(workspace: JournalingWorkspace(journal: Journal()), delivery: delivery)

        try await provider.restore(.managed(uuid: "too-late"))
        await provider.redeliverLifecycleRecords()
        await provider.redeliverLifecycleRecords()

        #expect(await delivery.delivered == [
            "too-late",
            "held", "transient", "refused",
            "held", "transient",
        ])
    }

    @Test("a redelivery whose asset list cannot be read sends nothing; with no port there is nothing to send")
    func unreadableListSendsNothing() async throws {
        let delivery = ScriptedLifecycleDelivery(held: ["a"], listFailure: ScriptedFailure())
        let provider = makeProvider(workspace: JournalingWorkspace(journal: Journal()), delivery: delivery)

        await provider.redeliverLifecycleRecords()
        let unported = makeProvider(workspace: JournalingWorkspace(journal: Journal()), delivery: nil)
        await unported.redeliverLifecycleRecords()
        try await unported.delete([.managed(uuid: "a")])

        #expect(await delivery.delivered.isEmpty)
    }
}

/// Every trash transition is the workspace's: the provider sends the verb and
/// writes nothing to the catalog itself.
@Suite("ManagedProvider routes the trash through the workspace")
struct ManagedProviderWorkspaceRoutingTests {
    /// Records each verb it receives; a purge answers with a re-issued delete.
    private actor RecordingWorkspace: ManagedWorkspace {
        private(set) var calls: [String] = []
        private let restoreRefusal: TrashRefusal?

        init(restoreRefusal: TrashRefusal? = nil) {
            self.restoreRefusal = restoreRefusal
        }

        func sealAsset(fileName: String, bytes _: Data) -> String {
            calls.append("seal \(fileName)")
            return "sealed"
        }

        func deleteAsset(id: String, retainDays: UInt32) {
            calls.append("delete \(id) \(retainDays)")
        }

        func restoreAsset(id: String) throws {
            calls.append("restore \(id)")
            if let restoreRefusal { throw restoreRefusal }
        }

        func purgeAsset(id: String) -> ManagedPurgeOutcome {
            calls.append("purge \(id)")
            return ManagedPurgeOutcome(assetID: id, reissueRecord: Data([0xA1]))
        }
    }

    private func makeProvider(_ workspace: RecordingWorkspace, catalog: MockCatalog) -> ManagedProvider {
        ManagedProvider(
            library: ManagedLibrary(
                layout: ManagedLibraryLayout(root: URL(filePath: "/capsule/Library")),
                fileStore: MockFileStore(),
                catalog: catalog,
                workspace: workspace
            ),
            authGate: MockLocalAuthGate()
        )
    }

    @Test("delete carries the default window; restore and purge go through unchanged; foreign ids are skipped")
    func verbsReachTheWorkspace() async throws {
        let workspace = RecordingWorkspace()
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "a"))
        let provider = makeProvider(workspace, catalog: catalog)

        try await provider.delete([.managed(uuid: "a"), .photoKit(localIdentifier: "PK/L0/001")])
        try await provider.restore(.managed(uuid: "a"))
        try await provider.purge(.managed(uuid: "a"))

        #expect(await workspace.calls == ["delete a 30", "restore a", "purge a"])
        // The catalog row is untouched: only the workspace writes the trash.
        #expect(await catalog.asset(id: "a")?.isDeleted == false)
    }

    @Test("a restore the workspace refuses is thrown to the caller")
    func refusedRestoreIsThrown() async {
        let workspace = RecordingWorkspace(restoreRefusal: .retentionElapsed(retentionUntil: "2024-08-02T00:00:00Z"))
        let provider = makeProvider(workspace, catalog: MockCatalog())

        await #expect(throws: TrashRefusal.retentionElapsed(retentionUntil: "2024-08-02T00:00:00Z")) {
            try await provider.restore(.managed(uuid: "a"))
        }
    }
}
