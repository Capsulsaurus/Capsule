import Foundation

/// The catalog contract — the read/write interface to the portable SQLite
/// catalog of assets, stacks, and albums.
///
/// This protocol *is* the catalog boundary: ``CapsuleCatalog`` implements it
/// over the Rust core, and `MockCatalog` (in `CapsuleTestSupport`) implements
/// it in memory so every consumer — `ManagedStore`, `AssetKit`, feature view
/// models — is tested against this contract rather than against SQLite.
///
/// All methods are `async`: the real implementation confines blocking SQLite
/// calls to an actor, off the main thread. The documented behaviour below is
/// binding on every conformer.
///
/// ### Timeline semantics
/// ``timeline(filter:offset:limit:)`` returns assets ordered by their
/// **effective capture instant** (``CatalogAsset/effectiveCaptureTimestamp`` —
/// the UTC capture time when known, else the device-local wall-clock)
/// **descending**, excluding any asset that is soft-deleted (`isDeleted`) or
/// hidden as a non-cover stack member (`isStackHidden`). A ``TimelineFilter``
/// capture-time window bounds that same instant, inclusive at both ends.
///
/// ### Gated views (SR1)
/// ``trash(offset:limit:)`` is the *Recently Deleted view* and is gated: it
/// throws `CatalogError.ViewLocked` until ``unlockView(_:using:)`` mints a
/// grant, which then covers a short grace window (5 minutes, per-view). Grants
/// never cross views — one for Hidden does not open Recently Deleted.
/// ``expiredTrash(olderThanSeconds:)`` is deliberately *ungated*: it is the
/// unattended retention sweep's input, with no user present to authenticate.
/// Do not build a user-facing listing on it.
///
/// The gate is view-time UX protection against a borrowed-unlocked-phone snoop;
/// it is **not** a cryptographic boundary.
public protocol AssetCatalog: Sendable {
    /// The `PRAGMA user_version` of the open catalog.
    func schemaVersion() async throws -> UInt32

    // MARK: Assets

    /// Insert a new asset. Throws if an asset with the same `id` already exists.
    func insertAsset(_ asset: CatalogAsset) async throws

    /// Insert the asset, or replace the existing row with the same `id`.
    func upsertAsset(_ asset: CatalogAsset) async throws

    /// The asset with the given catalog UUID, or `nil` if there is none.
    func asset(id: String) async throws -> CatalogAsset?

    /// The asset whose file bytes hash to `hashSHA256`, or `nil` — the dedup
    /// lookup the import pipeline uses to skip already-imported files.
    func asset(hashSHA256: String) async throws -> CatalogAsset?

    /// A windowed page of the timeline. See *Timeline semantics* above.
    func timeline(filter: TimelineFilter, offset: Int, limit: Int) async throws -> [CatalogAsset]

    /// Soft-delete an asset: mark it `isDeleted` with `deletedAt`, removing it
    /// from the timeline. The row and its file are retained until purged.
    func softDeleteAsset(id: String, deletedAt: Int64) async throws

    /// Restore a soft-deleted asset back into the timeline.
    func restoreAsset(id: String) async throws

    /// Soft-deleted assets whose `deletedAt` is older than `olderThanSeconds`
    /// before now — the trash-purge candidate set.
    func expiredTrash(olderThanSeconds: Int64) async throws -> [CatalogAsset]

    /// A windowed page of the trash — every soft-deleted asset, most-recently-
    /// deleted first. The Recently Deleted listing.
    ///
    /// **Gated (SR1).** Throws `CatalogError.ViewLocked` unless
    /// ``unlockView(_:using:)`` has minted a live ``GatedView/recentlyDeleted``
    /// grant. See *Gated views* below.
    func trash(offset: Int, limit: Int) async throws -> [CatalogAsset]

    /// Permanently remove an asset row. The on-disk file is the caller's concern.
    func purgeAsset(id: String) async throws

    // MARK: Stacks

    /// Insert a new stack.
    func insertStack(_ stack: CatalogStack) async throws

    /// Insert a new stack-membership row.
    func insertStackMember(_ member: CatalogStackMember) async throws

    /// Set whether an asset is hidden from the timeline as a stack member.
    func updateStackHidden(assetID: String, hidden: Bool) async throws

    /// Change which asset is a stack's primary representative.
    func updateStackPrimary(stackID: String, primaryAssetID: String) async throws

    /// The membership rows of a stack, ordered by `sequenceOrder` ascending.
    func stackMembers(stackID: String) async throws -> [CatalogStackMember]

    // MARK: Albums

    /// Insert a new user album.
    func insertAlbum(_ album: CatalogAlbum) async throws

    /// Update an existing user album.
    func updateAlbum(_ album: CatalogAlbum) async throws

    /// Delete a user album. Member assets survive; their `albumID` is cleared.
    func deleteAlbum(id: String) async throws

    /// The album with the given UUID, or `nil`.
    func album(id: String) async throws -> CatalogAlbum?

    /// Every user album, ordered by `createdAt` descending.
    func albums() async throws -> [CatalogAlbum]

    /// Set (or, with `nil`, clear) the album a managed asset belongs to.
    func setAssetAlbum(assetID: String, albumID: String?) async throws

    /// A windowed page of an album's assets, ordered as the timeline is.
    func albumAssets(albumID: String, offset: Int, limit: Int) async throws -> [CatalogAsset]

    // MARK: Gated views

    /// Challenge `gate` and, on success, mint a fresh-local-auth grant for `view`.
    ///
    /// A grant already live within its grace window is reused without a second
    /// challenge, and is **not** slid forward — the window is measured from the
    /// original mint, so re-entering a view cannot hold it open indefinitely.
    /// A refusal mints nothing, leaves the view locked, and throws the
    /// platform's `LocalAuthError` unchanged.
    func unlockView(_ view: GatedView, using gate: any LocalAuthGate) async throws

    /// Whether `view` currently holds a live grant. A pure state check — it
    /// never challenges the platform.
    func isViewUnlocked(_ view: GatedView) async -> Bool

    /// Revoke the grant for one view.
    func relockView(_ view: GatedView) async

    /// Revoke every grant — for the app being backgrounded or the device
    /// locking. The next ``unlockView(_:using:)`` re-authenticates.
    func lockViews() async
}

public extension AssetCatalog {
    /// A windowed page of the unfiltered timeline.
    func timeline(offset: Int, limit: Int) async throws -> [CatalogAsset] {
        try await timeline(filter: .all, offset: offset, limit: limit)
    }
}
