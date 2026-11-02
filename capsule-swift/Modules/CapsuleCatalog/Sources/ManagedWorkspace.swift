import Foundation

/// The writes of a managed library: sealing an import and every trash
/// transition.
///
/// The catalog only reads. Everything that changes what the library holds goes
/// through this port, because on the FFI lane each of these is a signed
/// provenance record in the Rust workspace (`FfiWorkspace.sealAsset`,
/// `deleteAsset`, `restoreAsset`, `purgeAsset`): the index row is a projection
/// of that record, so the change syncs and survives an index rebuild. A write
/// to the catalog row alone would sign nothing, and a rebuild would undo it.
///
/// The retention window is the workspace's. A conformer judges it, refuses a
/// late restore with ``TrashRefusal/retentionElapsed(retentionUntil:)``, and no
/// caller keeps retention logic of its own.
///
/// `CapsuleCatalogFFI` conforms over `FfiWorkspace`; `ManagedStore`'s
/// `LocalManagedWorkspace` is the mock lane's and the tests' conformer.
public protocol ManagedWorkspace: Sendable {
    /// Seal `bytes` into the library as a new asset and return its id.
    ///
    /// `fileName` is consulted for its extension. The workspace derives the
    /// asset's metadata from the bytes themselves.
    func sealAsset(fileName: String, bytes: Data) async throws -> String

    /// Move an asset to the trash with a retention window of `retainDays` days
    /// from now. An asset already in the trash keeps the window it was deleted
    /// with, so a retried delete never extends it.
    ///
    /// - Throws: ``TrashRefusal/notFound`` for an asset the library does not
    ///   hold.
    func deleteAsset(id: String, retainDays: UInt32) async throws

    /// Restore a trashed asset to the timeline.
    ///
    /// - Throws: ``TrashRefusal/notFound``, ``TrashRefusal/notInTrash``, or
    ///   ``TrashRefusal/retentionElapsed(retentionUntil:)`` at or after the
    ///   window's end. A refusal changes nothing.
    func restoreAsset(id: String) async throws

    /// Permanently remove a trashed asset: its original, its derivatives and
    /// cache entries, its sidecar and its index row.
    ///
    /// - Throws: ``TrashRefusal/notFound``, ``TrashRefusal/notInTrash`` (a purge
    ///   never skips the trash), or the store's error when a file cannot be
    ///   removed, in which case the asset stays in the trash for a retry.
    func purgeAsset(id: String) async throws -> ManagedPurgeOutcome
}

/// What a ``ManagedWorkspace/purgeAsset(id:)`` did.
public struct ManagedPurgeOutcome: Sendable, Equatable {
    /// The purged asset.
    public var assetID: String
    /// The signed shorter-retention `delete` an early purge appended, as the
    /// opaque canonical CBOR provenance record. It belongs on the album's ops
    /// surface so the server may purge too; the workspace keeps it on the
    /// asset's chain, so a delivery can be retried later. `nil` when the window
    /// had closed, or when the conformer signs nothing.
    public var reissueRecord: Data?

    public init(assetID: String, reissueRecord: Data? = nil) {
        self.assetID = assetID
        self.reissueRecord = reissueRecord
    }
}

/// Delivers a purged managed asset's `delete` to its album's ops surface.
///
/// Declared beside ``ManagedWorkspace`` rather than with `AssetKit`'s
/// `ManagedProvider`, which drives it, so `CapsuleCatalogFFI` can conform
/// without depending on the provider's module.
///
/// On the FFI lane a conformer reads the asset's head off the sync feed and
/// calls `FfiSession.deliverPurgeAtHash`, which sends what the server lacks of
/// the chain the workspace kept: an early purge's re-issued `delete` together
/// with any `delete` it chains onto. A purge whose kept head the feed already
/// lists is answered delivered without a send. Everything is read from the workspace, so a
/// delivery that failed is retried by calling it again, also after a relaunch.
public protocol PurgeDelivering: Sendable {
    /// Deliver the purged asset `assetID`'s `delete`. Throws only when the
    /// delivery could not be attempted (a refused session, an unreadable feed,
    /// a bad argument).
    func deliverPurge(assetID: String) async throws -> PurgeDelivery

    /// Every asset the workspace has purged (`FfiWorkspace.purgedAssetIds`):
    /// what `ManagedProvider.redeliverPurges()` walks.
    func purgedAssetIDs() async throws -> [String]
}

/// What became of one ``PurgeDelivering/deliverPurge(assetID:)``.
public enum PurgeDelivery: Sendable, Equatable {
    /// The server applied the purge's `delete`, or had already.
    case delivered
    /// Nothing was sent: the server has not indexed the asset's create, and it
    /// never sees a `delete` for an asset it has not indexed.
    case held
    /// The server refused it or could not be reached. A `permanent` failure is
    /// one the server judged final, so `ManagedProvider.redeliverPurges()` does
    /// not send it again for the rest of the provider's life. The refusal is
    /// not persisted: the first walk after a relaunch sends it once more, and
    /// the server refuses it again without applying anything.
    case failed(permanent: Bool)
}

/// Why a ``ManagedWorkspace`` trash verb was refused. Nothing was written.
public enum TrashRefusal: Error, Sendable, Equatable {
    /// The library holds no such asset.
    case notFound
    /// The asset is not in the trash: a restore has nothing to undo, and a
    /// purge never skips the trash.
    case notInTrash
    /// The retention window closed at `retentionUntil` (RFC 3339), so the asset
    /// can no longer be restored. It stays in the trash until it is purged.
    case retentionElapsed(retentionUntil: String?)
}
