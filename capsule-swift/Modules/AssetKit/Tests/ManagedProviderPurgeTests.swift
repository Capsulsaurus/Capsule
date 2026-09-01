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

    private func makeAsset(id: String) -> CatalogAsset {
        CatalogAsset(
            id: id,
            assetType: "photo",
            captureTimestamp: Self.captureTimestamp,
            importTimestamp: Self.captureTimestamp,
            hashSHA256: String(repeating: "a", count: 64)
        )
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
