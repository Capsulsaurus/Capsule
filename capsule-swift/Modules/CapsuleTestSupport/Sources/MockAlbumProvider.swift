import AssetKit
import CapsuleFoundation
import Foundation

/// An in-memory ``AlbumProvider`` for tests.
public actor MockAlbumProvider: AlbumProvider {
    private var albums: [AlbumSummary]
    private var assetsByAlbum: [AlbumID: [Asset]]
    private var continuation: AsyncStream<Void>.Continuation?

    /// The asset identifiers passed to ``addAsset(_:to:)``, for assertions.
    public private(set) var addedAssetIDs: [AssetID] = []

    public init(albums: [AlbumSummary] = [], assetsByAlbum: [AlbumID: [Asset]] = [:]) {
        self.albums = albums
        self.assetsByAlbum = assetsByAlbum
    }

    public func loadAlbums() -> [AlbumSummary] {
        albums
    }

    public func assets(in albumID: AlbumID) -> [Asset] {
        assetsByAlbum[albumID] ?? []
    }

    public func createUserAlbum(named name: String) {
        albums.append(AlbumSummary(id: .managed(uuid: UUID().uuidString), title: name, count: 0))
        continuation?.yield(())
    }

    public func addAsset(_ assetID: AssetID, to _: AlbumID) {
        addedAssetIDs.append(assetID)
        continuation?.yield(())
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        AsyncStream { continuation in
            Task { await self.attach(continuation) }
        }
    }

    private func attach(_ continuation: AsyncStream<Void>.Continuation) {
        self.continuation = continuation
    }
}
