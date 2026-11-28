import AssetKit
import CapsuleCatalog
import CapsuleFoundation
import CapsuleTestSupport
import Foundation
import ManagedStore
import Testing

/// A failure a scripted port throws.
private struct ScriptedFailure: Error {}

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
