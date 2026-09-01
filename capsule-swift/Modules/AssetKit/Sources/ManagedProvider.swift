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
    private static let timelineLimit = 10000

    private let library: ManagedLibrary
    private let authGate: any LocalAuthGate
    private var observers: [UUID: AsyncStream<AssetChange>.Continuation] = [:]

    /// - Parameter authGate: the SR1 fresh-local-auth adapter used to open the
    ///   Recently Deleted view. Defaults to the real `LAContext` gate; tests and
    ///   previews inject a scripted one, since no test can answer a Face ID
    ///   sheet.
    public init(library: ManagedLibrary, authGate: any LocalAuthGate = LocalAuthenticationGate()) {
        self.library = library
        self.authGate = authGate
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

    public func unlockTrash() async throws {
        let catalog = try await library.catalog()
        try await catalog.unlockView(.recentlyDeleted, using: authGate)
    }

    public func isTrashUnlocked() async -> Bool {
        guard let catalog = try? await library.catalog() else { return false }
        return await catalog.isViewUnlocked(.recentlyDeleted)
    }

    public func restore(_ id: AssetID) async throws {
        guard case let .managed(uuid) = id else { return }
        let catalog = try await library.catalog()
        try await catalog.restoreAsset(id: uuid)
        await emitReload()
    }

    /// Permanently remove an asset: its bytes first, then its catalog row.
    ///
    /// Bytes first is the load-bearing part. This is the operation a user reaches
    /// for when they want a photograph *gone*, so a failure to delete the file has
    /// to surface as a failure — not as a vanished row over bytes still on disk,
    /// which would report success while leaving the thing the user was trying to
    /// destroy. Throwing here leaves the asset in Recently Deleted, where it can be
    /// tried again.
    ///
    /// The reverse order's failure — a row pointing at bytes that are gone — is a
    /// broken thumbnail. This one's is a privacy hole, and they are not comparable.
    public func purge(_ id: AssetID) async throws {
        guard case let .managed(uuid) = id else { return }
        let catalog = try await library.catalog()
        // Read the row before dropping it: the media partition is derived from the
        // capture timestamp, so once the row is gone the directory is unknowable.
        if let asset = try await catalog.asset(id: uuid) {
            let captureDate = Date(timeIntervalSince1970: TimeInterval(asset.captureTimestamp))
            try await library.removeAssetFiles(uuid: uuid, captureDate: captureDate)
        }
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
