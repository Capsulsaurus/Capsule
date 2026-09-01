import CapsuleFoundation
import Foundation

/// Recently-Deleted operations over a soft-delete-capable backing store.
///
/// Capsule's managed library soft-deletes (the row and file linger until
/// purged), so it can list, restore, and permanently remove trashed assets.
/// PhotoKit deletions go to the *system* Recently Deleted, which third-party
/// apps cannot enumerate — so Capsule's trash covers managed assets.
/// ### Fresh local authentication (SR1)
/// Recently Deleted is a *gated view*: ``trashedAssets()`` throws until
/// ``unlockTrash()`` has taken a grant, which then covers a short grace window
/// (5 minutes). A screen listing the trash must request the grant before it
/// lists — the same shape the Hidden screen already uses.
///
/// The gate is view-time UX protection against a borrowed-unlocked-phone snoop;
/// it is **not** a cryptographic boundary, and it protects the *view*, not the
/// bytes.
public protocol TrashProvider: Sendable {
    /// Every soft-deleted asset, most-recently-deleted first.
    ///
    /// Throws unless a live fresh-auth grant exists — see *Fresh local
    /// authentication* above.
    func trashedAssets() async throws -> [Asset]

    /// Restore a trashed asset back into the timeline.
    func restore(_ id: AssetID) async throws

    /// Permanently remove a trashed asset.
    func purge(_ id: AssetID) async throws

    /// Challenge the device owner and, on success, take the grant that opens
    /// Recently Deleted. Throws the platform's refusal unchanged; a live grant
    /// is reused without a second prompt.
    func unlockTrash() async throws

    /// Whether a live grant already exists — checked before prompting, so
    /// re-entering the screen inside the grace window is silent.
    func isTrashUnlocked() async -> Bool
}
