import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - AlbumPort

/// Container albums — the real cryptographic unit.
///
/// Every method here is, on the far side, an MLS operation or a signed
/// lifecycle action. Two consequences the UI must respect and this protocol is
/// shaped to make obvious:
///
/// - **Membership changes are commits, not settings writes.** Adding a member
///   bumps the AMK epoch. `async throws` is not decoration: these can take real
///   time and can genuinely fail partway.
/// - **A view album is never a destination.** Only ``ContainerAlbum`` appears in
///   the writing methods, so passing a smart album to `move(_:to:)` does not
///   type-check.
public protocol AlbumPort: Sendable {
    /// Every container album the user is a member of.
    ///
    /// Maps to `albums.list_containers`.
    func containerAlbums() async throws -> [ContainerAlbum]

    /// One container album.
    ///
    /// Maps to `albums.get_container`.
    func containerAlbum(_ id: AlbumID) async throws -> ContainerAlbum?

    /// The derived, key-free views: All, Trash, Hidden, Quarantine, plus the
    /// user's smart albums and any aggregated albums.
    ///
    /// Maps to `library.list_views`.
    func viewAlbums() async throws -> [ViewAlbum]

    /// The album an import lands in when the user picks none, with the rule
    /// that resolved it recorded so a surprising destination is explainable.
    ///
    /// **Always resolves to a container.** Maps to
    /// `library.resolve_default_album`.
    func resolveDefaultAlbum(for scope: ImportScope?) async throws -> (album: ContainerAlbum, rule: ImportPlan.DestinationRule)

    /// Create a container album. Its policy is fixed here and afterwards
    /// changeable only through an upgrade ceremony.
    ///
    /// Maps to `albums.create`.
    func createAlbum(name: String, policy: AlbumPolicy) async throws -> ContainerAlbum

    /// Rename an album. Metadata-only.
    ///
    /// Maps to `albums.rename`.
    func renameAlbum(_ id: AlbumID, to name: String) async throws

    /// Set the album's cover.
    ///
    /// Maps to `albums.set_cover`.
    func setCoverAsset(_ assetID: AssetID?, for albumID: AlbumID) async throws

    /// Delete an album. **Refused for the currently-designated default** — the
    /// user must repoint first, so import always has a home.
    ///
    /// Maps to `albums.delete`.
    func deleteAlbum(_ id: AlbumID) async throws

    /// Move assets into a container album.
    ///
    /// A signed lifecycle action naming `(asset, target album, epoch)`, and
    /// **idempotent**: replaying it finds the target state already in place and
    /// no-ops, so a retry after a dropped connection is safe.
    ///
    /// Maps to `albums.move_assets`.
    func move(_ assetIDs: [AssetID], to albumID: AlbumID) async throws

    /// Invite a user by handle, at a role. Issues an MLS `Add` for every one of
    /// their devices and bumps the epoch.
    ///
    /// Maps to `albums.invite_member`.
    func inviteMember(handle: String, role: AlbumRole, to albumID: AlbumID) async throws

    /// Change a member's role. An MLS commit; bumps the epoch.
    ///
    /// Maps to `albums.set_member_role`.
    func setMemberRole(_ role: AlbumRole, for handle: String, in albumID: AlbumID) async throws

    /// Remove a member. An MLS `Remove` plus an epoch bump, so they derive no
    /// future key.
    ///
    /// Maps to `albums.remove_member`.
    func removeMember(handle: String, from albumID: AlbumID) async throws

    /// A stream that fires when the album set or any album's membership
    /// changes.
    func changes() -> AsyncStream<Void>
}

// MARK: - SmartAlbumPort

/// User-defined smart albums.
///
/// A definition is one LWW register in the E2E-encrypted library-settings
/// document, so every write here is a single stamped operation and there is
/// never a partial-predicate merge. Membership is **computed, never stored**:
/// nothing in this port moves or re-encrypts an asset.
public protocol SmartAlbumPort: Sendable {
    /// Every smart-album definition, including ones this build cannot evaluate.
    ///
    /// Definitions ahead of this build's grammar are returned with
    /// ``SmartAlbumDefinition/isEvaluable`` false and must be **preserved
    /// verbatim, never stripped** — the UI shows them as "created by a newer
    /// app version" rather than hiding or deleting them.
    ///
    /// Maps to `settings.list_smart_albums`.
    func definitions() async throws -> [SmartAlbumDefinition]

    /// One definition.
    ///
    /// Maps to `settings.get_smart_album`.
    func definition(_ id: SmartAlbumID) async throws -> SmartAlbumDefinition?

    /// Create or replace a definition.
    ///
    /// Validates through ``PredicateValidator`` before writing; an invalid
    /// predicate is a **structural rejection**, never a tolerated definition.
    ///
    /// Maps to `settings.put_smart_album`.
    func save(_ definition: SmartAlbumDefinition) async throws

    /// Delete a definition — a stamped tombstone in the register, not a row
    /// removal, so the deletion converges with a concurrent edit.
    ///
    /// Maps to `settings.delete_smart_album`.
    func delete(_ id: SmartAlbumID) async throws

    /// Evaluate a definition into a page of assets.
    ///
    /// A pure function of `(definition, decryptable assets)` processed in
    /// sorted asset-id order, so the same window is byte-identical on every
    /// device.
    ///
    /// Maps to `library.evaluate_smart_album`.
    func evaluate(_ id: SmartAlbumID, offset: Int, limit: Int) async throws -> Page<LibraryAsset>

    /// Preview a predicate without saving it — what the editor's live result
    /// count reads.
    ///
    /// Maps to `library.evaluate_predicate`.
    func preview(_ predicate: SmartAlbumPredicate, limit: Int) async throws -> Page<LibraryAsset>

    /// A stream that fires when any definition changes, including from another
    /// device's sync.
    func changes() -> AsyncStream<Void>
}
