import CapsuleCatalog
import CapsuleFoundation
import Foundation
import ManagedStore

/// The ``AlbumProvider`` over editable Capsule user albums, stored in the
/// catalog's `albums` table with membership in `assets.album_id`.
public actor ManagedAlbumProvider: AlbumProvider {
    private static let albumAssetLimit = 10_000

    private let library: ManagedLibrary
    private var observers: [UUID: AsyncStream<Void>.Continuation] = [:]

    public init(library: ManagedLibrary) {
        self.library = library
    }

    public func loadAlbums() async -> [AlbumSummary] {
        guard let catalog = try? await library.catalog(),
              let albums = try? await catalog.albums()
        else {
            return []
        }
        var summaries: [AlbumSummary] = []
        for album in albums {
            let rows = (try? await catalog.albumAssets(
                albumID: album.id, offset: 0, limit: Self.albumAssetLimit
            )) ?? []
            summaries.append(AlbumSummary(
                id: .managed(uuid: album.id),
                title: album.name,
                count: rows.count,
                coverAssetID: rows.first.map { .managed(uuid: $0.id) }
            ))
        }
        return summaries
    }

    public func assets(in albumID: AlbumID) async throws -> [Asset] {
        guard case let .managed(uuid) = albumID else { return [] }
        let catalog = try await library.catalog()
        let rows = try await catalog.albumAssets(albumID: uuid, offset: 0, limit: Self.albumAssetLimit)
        return rows.map(Asset.init(catalogAsset:))
    }

    public func createUserAlbum(named name: String) async throws {
        let catalog = try await library.catalog()
        let now = Int64(Date().timeIntervalSince1970)
        try await catalog.insertAlbum(CatalogAlbum(
            id: UUIDv7.string(),
            name: name,
            createdAt: now,
            modifiedAt: now
        ))
        emitChange()
    }

    public func addAsset(_ assetID: AssetID, to albumID: AlbumID) async throws {
        guard case let .managed(albumUUID) = albumID else { throw AlbumError.readOnly }
        // Only managed assets can belong to a Capsule user album.
        guard case let .managed(assetUUID) = assetID else { return }
        let catalog = try await library.catalog()
        try await catalog.setAssetAlbum(assetID: assetUUID, albumID: albumUUID)
        emitChange()
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        AsyncStream { continuation in
            let token = UUID()
            Task { await self.register(continuation, token: token) }
            continuation.onTermination = { _ in
                Task { await self.unregister(token) }
            }
        }
    }

    private func register(_ continuation: AsyncStream<Void>.Continuation, token: UUID) {
        observers[token] = continuation
    }

    private func unregister(_ token: UUID) {
        observers[token] = nil
    }

    private func emitChange() {
        for continuation in observers.values {
            continuation.yield(())
        }
    }
}
