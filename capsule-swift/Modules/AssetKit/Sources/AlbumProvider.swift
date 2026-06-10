import CapsuleFoundation
import Foundation

/// A lightweight description of an album for the albums list.
public struct AlbumSummary: Identifiable, Sendable, Equatable, Hashable {
    /// The source-tagged album identifier.
    public var id: AlbumID
    /// The album's display title.
    public var title: String
    /// The number of assets in the album.
    public var count: Int
    /// The asset to show as the album's cover, when known.
    public var coverAssetID: AssetID?

    public init(id: AlbumID, title: String, count: Int, coverAssetID: AssetID? = nil) {
        self.id = id
        self.title = title
        self.count = count
        self.coverAssetID = coverAssetID
    }

    /// Whether the album is a user-editable Capsule album.
    public var isUserAlbum: Bool { id.isUserAlbum }
}

/// A source of albums.
///
/// `PhotoKitAlbumProvider` exposes the system smart albums (read-only);
/// `ManagedAlbumProvider` exposes editable Capsule user albums; and
/// `CompositeAlbumProvider` merges both for the albums screen. Editing calls
/// on a read-only album throw ``AlbumError/readOnly``.
public protocol AlbumProvider: Sendable {
    /// Every album this provider exposes.
    func loadAlbums() async -> [AlbumSummary]

    /// The assets in an album, newest first.
    func assets(in albumID: AlbumID) async throws -> [Asset]

    /// Create a new, empty user album.
    func createUserAlbum(named name: String) async throws

    /// Add an asset to a user album.
    func addAsset(_ assetID: AssetID, to albumID: AlbumID) async throws

    /// A stream that fires whenever the album set changes.
    func changes() -> AsyncStream<Void>
}

/// Errors raised by album providers.
public enum AlbumError: Error, Sendable, Equatable {
    /// The album cannot be edited (a system smart album).
    case readOnly
    /// No album exists for the given identifier.
    case notFound
}
