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

    private func makeProvider(catalog: MockCatalog, fileStore: MockFileStore) -> ManagedProvider {
        ManagedProvider(
            library: ManagedLibrary(layout: layout, fileStore: fileStore, catalog: catalog),
            authGate: MockLocalAuthGate()
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
