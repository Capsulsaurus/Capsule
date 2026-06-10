import CapsuleCatalog
import CapsuleFoundation
import Foundation
import ManagedStore

/// The ``AssetProvider`` over the Capsule-managed library.
///
/// Reads its timeline from the catalog and maps each ``CatalogAsset`` to the
/// source-agnostic ``Asset``. Mutations (favourite, delete) write straight to
/// the catalog; deletion is a soft delete, leaving the file in place. The
/// import flow calls ``refresh()`` once an import completes so the timeline
/// picks up the new assets.
public actor ManagedProvider: AssetProvider, TrashProvider {
    /// Upper bound on the managed timeline window for the prototype.
    private static let timelineLimit = 10_000

    private let library: ManagedLibrary
    private var observers: [UUID: AsyncStream<AssetChange>.Continuation] = [:]

    public init(library: ManagedLibrary) {
        self.library = library
    }

    public func authorizationStatus() -> AssetAuthorizationStatus {
        .authorized // The managed store needs no system permission.
    }

    @discardableResult
    public func requestAuthorization() -> AssetAuthorizationStatus {
        .authorized
    }

    public func loadTimeline() async throws -> any AssetSnapshot {
        let catalog = try await library.catalog()
        let rows = try await catalog.timeline(offset: 0, limit: Self.timelineLimit)
        return InMemoryAssetSnapshot(rows.map(Asset.init(catalogAsset:)))
    }

    public func asset(for id: AssetID) async throws -> Asset? {
        guard case let .managed(uuid) = id else { return nil }
        let catalog = try await library.catalog()
        return try await catalog.asset(id: uuid).map(Asset.init(catalogAsset:))
    }

    public nonisolated func changes() -> AsyncStream<AssetChange> {
        AsyncStream { continuation in
            let token = UUID()
            Task { await self.register(continuation, token: token) }
            continuation.onTermination = { _ in
                Task { await self.unregister(token) }
            }
        }
    }

    public func setFavorite(_ isFavorite: Bool, for id: AssetID) async throws {
        guard case let .managed(uuid) = id else { return }
        let catalog = try await library.catalog()
        guard var asset = try await catalog.asset(id: uuid) else { return }
        asset.rating = isFavorite ? 1 : 0
        try await catalog.upsertAsset(asset)
        await emitReload()
    }

    public func delete(_ ids: [AssetID]) async throws {
        let catalog = try await library.catalog()
        let deletedAt = Int64(Date().timeIntervalSince1970)
        for id in ids {
            guard case let .managed(uuid) = id else { continue }
            try await catalog.softDeleteAsset(id: uuid, deletedAt: deletedAt)
        }
        await emitReload()
    }

    /// Re-publish the timeline — called once an import has added assets.
    public func refresh() async {
        await emitReload()
    }

    // MARK: TrashProvider

    public func trashedAssets() async throws -> [Asset] {
        let catalog = try await library.catalog()
        let rows = try await catalog.trash(offset: 0, limit: Self.timelineLimit)
        return rows.map(Asset.init(catalogAsset:))
    }

    public func restore(_ id: AssetID) async throws {
        guard case let .managed(uuid) = id else { return }
        let catalog = try await library.catalog()
        try await catalog.restoreAsset(id: uuid)
        await emitReload()
    }

    public func purge(_ id: AssetID) async throws {
        guard case let .managed(uuid) = id else { return }
        let catalog = try await library.catalog()
        // Removes the catalog row; the on-disk file cleanup is a follow-up.
        try await catalog.purgeAsset(id: uuid)
        await emitReload()
    }

    // MARK: Private

    private func register(_ continuation: AsyncStream<AssetChange>.Continuation, token: UUID) {
        observers[token] = continuation
    }

    private func unregister(_ token: UUID) {
        observers[token] = nil
    }

    private func emitReload() async {
        guard !observers.isEmpty, let snapshot = try? await loadTimeline() else { return }
        for continuation in observers.values {
            continuation.yield(.reload(snapshot))
        }
    }
}
