import Foundation

/// A stable, source-tagged identifier for an album.
///
/// Like ``AssetID``, this unifies the hybrid model: an album is either a
/// read-only system smart album or a user-created Capsule album, and every
/// request round-trips through this identifier to the provider that owns it.
public enum AlbumID: Hashable, Sendable, Codable {
    /// A system Photos smart album, keyed by `PHAssetCollection.localIdentifier`.
    case smart(localIdentifier: String)

    /// A Capsule user album, keyed by its catalog UUID.
    case managed(uuid: String)
}

public extension AlbumID {
    /// Whether this is a user-editable Capsule album.
    var isUserAlbum: Bool {
        if case .managed = self { return true }
        return false
    }
}
