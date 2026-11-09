import Foundation
import Testing

import CapsuleCatalog
import CapsuleTestSupport
import ManagedStore

/// The mock lane's workspace keeps the FFI workspace's trash contract: a
/// delete opens a window, a late restore is refused, and a purge never skips
/// the trash and takes the files before the row.
@Suite("LocalManagedWorkspace trash state machine")
struct LocalManagedWorkspaceTrashTests {
    private let layout = ManagedLibraryLayout(root: URL(filePath: "/capsule/Library"))
    // 1_720_000_000 → 2024-07-03 UTC, so the partition is media/2024/2024-07.
    private let captureDate = Date(timeIntervalSince1970: 1720000000)
    private static let day: Int64 = 86400

    /// A clock a test moves forward.
    private final class Clock: @unchecked Sendable {
        private let lock = NSLock()
        private var value: Int64
        init(_ value: Int64) { self.value = value }
        var now: Int64 { lock.withLock { value } }
        func advance(by seconds: Int64) { lock.withLock { value += seconds } }
    }

    private func makeWorkspace(
        catalog: MockCatalog,
        fileStore: MockFileStore,
        clock: Clock = Clock(1720000000)
    ) -> LocalManagedWorkspace {
        LocalManagedWorkspace(
            layout: layout,
            fileStore: fileStore,
            catalog: catalog,
            hasher: MockContentHasher(),
            metadataExtractor: MockMetadataExtractor(),
            clock: { clock.now }
        )
    }

    private func seed(_ id: String, in catalog: MockCatalog) async throws {
        try await catalog.insertAsset(Fixtures.catalogAsset(id: id, captureTimestamp: 1720000000))
    }

    @Test("a delete trashes the row, a retried delete keeps it, and a restore brings it back")
    func deleteThenRestore() async throws {
        let catalog = MockCatalog()
        try await seed("a", in: catalog)
        let workspace = makeWorkspace(catalog: catalog, fileStore: MockFileStore())

        try await workspace.deleteAsset(id: "a", retainDays: 30)
        let deletedAt = try #require(await catalog.asset(id: "a")?.deletedAt)
        #expect(await catalog.timeline(filter: .all, offset: 0, limit: 10).isEmpty)
        try await workspace.deleteAsset(id: "a", retainDays: 1)
        #expect(await catalog.asset(id: "a")?.deletedAt == deletedAt)

        try await workspace.restoreAsset(id: "a")
        #expect(await catalog.timeline(filter: .all, offset: 0, limit: 10).map(\.id) == ["a"])
        await #expect(throws: TrashRefusal.notInTrash) {
            try await workspace.restoreAsset(id: "a")
        }
    }

    @Test("a restore at or after the window's end is refused and the asset stays trashed")
    func lateRestoreIsRefused() async throws {
        let catalog = MockCatalog()
        try await seed("a", in: catalog)
        let clock = Clock(1720000000)
        let workspace = makeWorkspace(catalog: catalog, fileStore: MockFileStore(), clock: clock)

        try await workspace.deleteAsset(id: "a", retainDays: 30)
        clock.advance(by: 30 * Self.day)

        await #expect(throws: TrashRefusal.self) {
            try await workspace.restoreAsset(id: "a")
        }
        #expect(await catalog.asset(id: "a")?.isDeleted == true)
    }

    @Test("an unknown asset is not found by every verb")
    func unknownAssetIsNotFound() async {
        let workspace = makeWorkspace(catalog: MockCatalog(), fileStore: MockFileStore())
        await #expect(throws: TrashRefusal.notFound) { try await workspace.deleteAsset(id: "x", retainDays: 30) }
        await #expect(throws: TrashRefusal.notFound) { try await workspace.restoreAsset(id: "x") }
        await #expect(throws: TrashRefusal.notFound) { _ = try await workspace.purgeAsset(id: "x") }
    }

    @Test("a purge never skips the trash")
    func purgeRefusesALiveAsset() async throws {
        let catalog = MockCatalog()
        try await seed("a", in: catalog)
        let workspace = makeWorkspace(catalog: catalog, fileStore: MockFileStore())

        await #expect(throws: TrashRefusal.notInTrash) { _ = try await workspace.purgeAsset(id: "a") }
        #expect(await catalog.asset(id: "a") != nil)
    }

    @Test("a purge removes the media file and its sidecar, then the row, and spares its neighbours")
    func purgeRemovesFilesThenRow() async throws {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        try await seed("asset-1", in: catalog)
        let media = layout.mediaFile(uuid: "asset-1", fileExtension: "heic", captureDate: captureDate)
        let sidecar = layout.sidecarFile(uuid: "asset-1", captureDate: captureDate)
        let neighbour = layout.mediaFile(uuid: "asset-2", fileExtension: "jpg", captureDate: captureDate)
        try await fileStore.write(Data("bytes".utf8), to: media)
        try await fileStore.write(Data("sidecar".utf8), to: sidecar)
        try await fileStore.write(Data("theirs".utf8), to: neighbour)
        let workspace = makeWorkspace(catalog: catalog, fileStore: fileStore)
        try await workspace.deleteAsset(id: "asset-1", retainDays: 30)

        let outcome = try await workspace.purgeAsset(id: "asset-1")

        #expect(outcome == ManagedPurgeOutcome(assetID: "asset-1"))
        #expect(await catalog.asset(id: "asset-1") == nil)
        #expect(await fileStore.fileExists(at: media) == false)
        #expect(await fileStore.fileExists(at: sidecar) == false)
        #expect(await fileStore.fileExists(at: neighbour))
    }

    @Test("a file that cannot be removed keeps the row in the trash, and throws")
    func failedRemovalKeepsTheRow() async throws {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        try await seed("asset-1", in: catalog)
        let media = layout.mediaFile(uuid: "asset-1", fileExtension: "heic", captureDate: captureDate)
        try await fileStore.write(Data("bytes".utf8), to: media)
        let workspace = makeWorkspace(catalog: catalog, fileStore: fileStore)
        try await workspace.deleteAsset(id: "asset-1", retainDays: 30)
        await fileStore.injectFailure(on: .remove)

        await #expect(throws: (any Error).self) { _ = try await workspace.purgeAsset(id: "asset-1") }

        #expect(await catalog.asset(id: "asset-1")?.isDeleted == true)
        #expect(await fileStore.fileExists(at: media))
    }
}
