import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PortBackedTrashProvider

/// The ``TrashProvider`` the Recently Deleted screen sees, over
/// ``OrganizePort``.
///
/// ## Why the listing does not come from `trashEntries`
///
/// ``OrganizePort/trashEntries(offset:limit:)`` is the natural-looking source,
/// but a ``TrashEntry`` carries only `(assetID, deletedAt, retentionUntil)` —
/// no media type, no dimensions, no capture date — so turning a page of them
/// into `[Asset]` would mean a second per-asset read anyway. Reading the trash
/// **slice** of the timeline instead (``VisibilitySlice/trash``) returns whole
/// ``LibraryAsset`` rows in one window, already ordered, and goes through the
/// same query path the rest of the app uses — so the trash view cannot drift
/// from the timeline's own idea of what is deleted.
///
/// `trashEntries` remains the right read for a screen that wants to show the
/// **signed retention deadline**, which is the one fact only it carries.
/// `TrashProvider` has nowhere to put a deadline, so this adapter does not
/// pretend to expose one.
public struct PortBackedTrashProvider: TrashProvider {
    /// Upper bound on the trash listing. `TrashProvider` returns a materialised
    /// array, so the bound is explicit rather than implied.
    public static let maximumTrashedAssets = 2000

    private let organize: any OrganizePort
    private let library: any LibraryPort

    public init(organize: any OrganizePort, library: any LibraryPort) {
        self.organize = organize
        self.library = library
    }

    /// Every soft-deleted asset, newest first.
    ///
    /// The port's canonical order is newest **capture** first, not
    /// most-recently-deleted first as `TrashProvider` documents. The two agree
    /// for the common case and the difference is not worth a full-slice sort
    /// through a second read; a screen that needs deletion order should read
    /// ``OrganizePort/trashEntries(offset:limit:)``, which carries `deletedAt`.
    public func trashedAssets() async throws -> [Asset] {
        let page = try await library.assets(
            matching: .trash,
            offset: 0,
            limit: Self.maximumTrashedAssets
        )
        return page.items.map(Asset.init(libraryAsset:))
    }

    /// Restore from trash, inside the retention window.
    ///
    /// Appends a provenance record; the original `delete` record survives, so
    /// the chain keeps "deleted on X, restored on Y".
    public func restore(_ id: AssetID) async throws {
        try await organize.restoreFromTrash([id])
    }

    /// Purge ahead of the signed retention deadline, at the user's explicit
    /// request. Irreversible for the bytes; the provenance chain survives as a
    /// tombstone-with-history.
    public func purge(_ id: AssetID) async throws {
        try await organize.purge([id])
    }
}
