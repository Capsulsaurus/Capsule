import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - StackPort

/// Asset stacks — RAW+JPEG pairs, bursts, Live Photos.
///
/// **Every operation here is metadata-only.** A stack edit rewrites the
/// `stack_membership` register on each member's sidecar and emits one
/// `metadata-update` per affected asset; it never deletes, rewrites, or merges
/// the underlying bytes. Even choosing a burst's "best photo" is a pointer
/// change, not a destructive act — which is why nothing in this protocol can
/// lose an original.
public protocol StackPort: Sendable {
    /// One stack, with its members in order and its derived cull state.
    ///
    /// Maps to `stacks.get`.
    func stack(_ id: StackID) async throws -> Stack?

    /// The assets in a stack, in `member_index` order, the primary included.
    ///
    /// Maps to `stacks.members`.
    func members(of id: StackID) async throws -> [LibraryAsset]

    /// Group assets into a new stack.
    ///
    /// Maps to `stacks.create`.
    func createStack(
        from assetIDs: [AssetID],
        type: StackType,
        primary: AssetID
    ) async throws -> StackID

    /// Add an asset to an existing stack.
    ///
    /// Maps to `stacks.add_member`.
    func addToStack(_ assetID: AssetID, stackID: StackID, role: StackRole) async throws

    /// Remove an asset from its stack — a stamped `nil` in its register, which
    /// converges with a concurrent stack edit from another device.
    ///
    /// Maps to `stacks.remove_member`.
    func removeFromStack(_ assetID: AssetID) async throws

    /// Promote a member to primary — the "best photo" pointer.
    ///
    /// Maps to `stacks.set_primary`.
    func setPrimary(_ assetID: AssetID, in stackID: StackID) async throws

    /// Dissolve a stack, leaving every member an independent asset.
    ///
    /// Maps to `stacks.unstack`.
    func unstack(_ id: StackID) async throws
}

// MARK: - OrganizePort

/// The per-asset metadata edits: rating, culling, hiding, tags, captions.
///
/// Every write here is a CRDT operation on the sidecar, so concurrent edits
/// from two devices converge with no conflict dialog. Two things the protocol
/// deliberately makes explicit:
///
/// - **Rating and cull are separate calls** because they are separate fields. A
///   reject can carry three stars; a single "quality" control would force a
///   lossy workflow.
/// - **An AI tag is dismissed by its add id**, not by its text. A remove that
///   names an add the replica never observed is *rejected*, not a no-op, so the
///   caller must pass the identity it actually saw.
public protocol OrganizePort: Sendable {
    /// Set the star rating, 0–5.
    ///
    /// Maps to `organize.set_rating`.
    func setRating(_ rating: UInt8, for assetIDs: [AssetID]) async throws

    /// Set the culling flag. Applied to a **collapsed stack** it flags every
    /// member, one `metadata-update` each, atomically staged — a group has no
    /// stored flag of its own.
    ///
    /// Maps to `organize.set_cull`.
    func setCull(_ flag: CullFlag, for assetIDs: [AssetID]) async throws

    /// Set the user-hidden flag.
    ///
    /// View-layer only: a hidden asset stays in its album, keeps syncing, and
    /// stays reachable from its stack and from any share it was already part
    /// of. This is **not** deletion and **not** access control.
    ///
    /// Maps to `organize.set_hidden`.
    func setHidden(_ hidden: Bool, for assetIDs: [AssetID]) async throws

    /// Add a user tag.
    ///
    /// Maps to `organize.add_user_tag`.
    func addUserTag(_ tag: String, to assetIDs: [AssetID]) async throws

    /// Remove a user tag by the add id that introduced it.
    ///
    /// - Throws: when the add id was never observed on this replica — the
    ///   "remove an element you never added" defence.
    ///
    /// Maps to `organize.remove_user_tag`.
    func removeUserTag(addID: AddID, from assetID: AssetID) async throws

    /// Promote an AI tag to a user tag — an explicit user action that copies
    /// the entry with a fresh user-scoped add id. **Never automatic.**
    ///
    /// Maps to `organize.promote_ai_tag`.
    func promoteAITag(addID: AddID, on assetID: AssetID, alsoRemoveFromAI: Bool) async throws

    /// Dismiss an AI tag by its original add id.
    ///
    /// Maps to `organize.dismiss_ai_tag`.
    func dismissAITag(addID: AddID, on assetID: AssetID) async throws

    /// Set the caption.
    ///
    /// A losing concurrent edit is preserved in the superseded log rather than
    /// silently clobbered — read it back through
    /// ``LibraryPort/sidecar(for:)``.
    ///
    /// Maps to `organize.set_caption`.
    func setCaption(_ caption: String?, for assetID: AssetID) async throws

    /// Restore a superseded caption, making it the current value with a fresh
    /// stamp.
    ///
    /// Maps to `organize.restore_caption`.
    func restoreCaption(_ superseded: Stamped<String>, for assetID: AssetID) async throws

    /// Set or clear the capture coordinate.
    ///
    /// The coordinate is stored **verbatim in its datum** and never converted at
    /// rest. A ``GpsSource/derived`` fix reaching this call means the user has
    /// explicitly confirmed it — an automated guess must never arrive here on
    /// its own.
    ///
    /// Maps to `organize.set_gps`.
    func setGps(_ gps: Gps?, for assetID: AssetID) async throws

    /// Soft-delete: move to trash with a signed retention window.
    ///
    /// Maps to `organize.delete`.
    func moveToTrash(_ assetIDs: [AssetID], retentionDays: Int?) async throws

    /// Restore from trash within the retention window.
    ///
    /// Appends a new provenance record; the original `delete` record is **not**
    /// removed, so the chain keeps "deleted on X, restored on Y".
    ///
    /// Maps to `organize.trash_restore`.
    func restoreFromTrash(_ assetIDs: [AssetID]) async throws

    /// Permanently purge, ahead of the retention deadline, at the user's
    /// explicit request.
    ///
    /// Irreversible for the bytes. The provenance chain survives as a
    /// tombstone-with-history.
    ///
    /// Maps to `organize.purge`.
    func purge(_ assetIDs: [AssetID]) async throws

    /// The trash, with each entry's retention deadline.
    ///
    /// Maps to `organize.list_trash`.
    func trashEntries(offset: Int, limit: Int) async throws -> Page<TrashEntry>
}
