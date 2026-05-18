import Foundation

/// A stable, source-tagged identifier for a photo or video asset.
///
/// `AssetID` is the unification primitive of the hybrid data layer: a timeline
/// mixes assets from the system Photos library and the Capsule-managed store,
/// and every per-asset request round-trips through this identifier back to the
/// provider that owns it.
public enum AssetID: Hashable, Sendable, Codable {
    /// An asset in the system Photos library, keyed by `PHAsset.localIdentifier`.
    case photoKit(localIdentifier: String)

    /// An asset in the Capsule-managed library, keyed by its catalog UUID.
    case managed(uuid: String)
}

public extension AssetID {
    /// Whether this asset is backed by the system Photos library.
    var isPhotoKit: Bool {
        if case .photoKit = self { return true }
        return false
    }

    /// Whether this asset is backed by the Capsule-managed store.
    var isManaged: Bool {
        if case .managed = self { return true }
        return false
    }
}
