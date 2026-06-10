import CapsuleFoundation
import Foundation

/// Recently-Deleted operations over a soft-delete-capable backing store.
///
/// Capsule's managed library soft-deletes (the row and file linger until
/// purged), so it can list, restore, and permanently remove trashed assets.
/// PhotoKit deletions go to the *system* Recently Deleted, which third-party
/// apps cannot enumerate — so Capsule's trash covers managed assets.
public protocol TrashProvider: Sendable {
    /// Every soft-deleted asset, most-recently-deleted first.
    func trashedAssets() async throws -> [Asset]

    /// Restore a trashed asset back into the timeline.
    func restore(_ id: AssetID) async throws

    /// Permanently remove a trashed asset.
    func purge(_ id: AssetID) async throws
}
