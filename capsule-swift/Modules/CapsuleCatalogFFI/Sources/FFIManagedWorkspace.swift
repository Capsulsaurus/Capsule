import CapsuleCatalog
import CapsuleDomain
import CapsuleFoundation
import Foundation
import os

/// The managed store's log. Every write is an INFO line naming its asset;
/// refusals are DEBUG.
private let managedLog = Logger(subsystem: CapsuleLog.subsystem, category: "managed-workspace")

// MARK: - Workspace seam

/// The import and trash verbs of `FfiWorkspace`, and nothing else of it.
///
/// Narrowed to a protocol so the adapter is tested over an in-memory double
/// rather than a passphrase-sealed workspace on disk; the generated object
/// conforms as declared.
protocol FFIManagedWorkspaceVerbs: Sendable {
    func defaultAlbumId() throws -> String
    func sealAsset(albumId: String, fileName: String, bytes: Data) throws -> String
    func deleteAsset(assetId: String, retainDays: UInt32) throws -> FfiTrashEntry
    func restoreAsset(assetId: String) throws
    func purgeAsset(assetId: String) throws -> FfiPurgeOutcome
}

extension FfiWorkspace: FFIManagedWorkspaceVerbs {}

// MARK: - FFIManagedWorkspace

/// The Rust-backed ``ManagedWorkspace``: an import is `FfiWorkspace.sealAsset`
/// and every trash transition is the workspace's signed verb, so each one
/// syncs and survives an index rebuild. The `FFIAssetCatalog` opened over the
/// same root's `index/library.sqlite` reads the rows those records project.
///
/// Imports are sealed into the account's default album, which enrollment
/// creates. `FfiError.Trash` becomes ``TrashRefusal``, so a trash view names the
/// same refusal on this lane and the mock one; any other failure is rethrown
/// as it came.
public struct FFIManagedWorkspace: ManagedWorkspace {
    private let workspace: any FFIManagedWorkspaceVerbs

    /// Write the managed store through `workspace`.
    public init(workspace: FfiWorkspace) {
        self.init(source: workspace)
    }

    /// Write through `source`. The seam tests drive.
    init(source: any FFIManagedWorkspaceVerbs) {
        workspace = source
    }

    public func sealAsset(fileName: String, bytes: Data) async throws -> String {
        let album = try workspace.defaultAlbumId()
        let assetID = try workspace.sealAsset(albumId: album, fileName: fileName, bytes: bytes)
        managedLog.info("sealed \(assetID, privacy: .public) into album \(album, privacy: .public), \(bytes.count) bytes")
        return assetID
    }

    public func deleteAsset(id: String, retainDays: UInt32) async throws {
        let entry = try translatingTrashRefusals { try workspace.deleteAsset(assetId: id, retainDays: retainDays) }
        managedLog.info(
            "trashed \(id, privacy: .public) until \(entry.retentionUntil ?? "never", privacy: .public)"
        )
    }

    public func restoreAsset(id: String) async throws {
        try translatingTrashRefusals { try workspace.restoreAsset(assetId: id) }
        managedLog.info("restored \(id, privacy: .public)")
    }

    public func purgeAsset(id: String) async throws -> ManagedPurgeOutcome {
        let outcome = try translatingTrashRefusals { try workspace.purgeAsset(assetId: id) }
        managedLog.info("purged \(id, privacy: .public), reissued=\(outcome.reissueRecord != nil, privacy: .public)")
        return ManagedPurgeOutcome(assetID: outcome.assetId, reissueRecord: outcome.reissueRecord)
    }
}

// MARK: - Refusals

/// Run one trash verb, rethrowing an `FfiError.Trash` as the ``TrashRefusal``
/// it names.
private func translatingTrashRefusals<T>(_ call: () throws -> T) throws -> T {
    do {
        return try call()
    } catch let FfiError.Trash(refusal, _, retentionUntil, detail) {
        managedLog.debug("trash verb refused: \(detail, privacy: .public)")
        throw TrashRefusal(refusal, retentionUntil: retentionUntil)
    }
}

extension TrashRefusal {
    /// The refusal an `FfiTrashRefusal` names.
    init(_ refusal: FfiTrashRefusal, retentionUntil: String?) {
        switch refusal {
        case .notFound: self = .notFound
        case .notInTrash: self = .notInTrash
        case .retentionElapsed: self = .retentionElapsed(retentionUntil: retentionUntil)
        }
    }
}

// MARK: - Trash entries

extension TrashEntry {
    /// The domain entry for a workspace trash entry: its signed window and the
    /// workspace's own judgement of whether a restore is accepted now, so no
    /// app re-implements the window. `nil` for an entry whose `deletedAt` is not
    /// RFC 3339; a `retentionUntil` that is not is read as no window, as the
    /// workspace reads it.
    init?(_ entry: FfiTrashEntry) {
        guard let deletedAt = CapsuleTimestamp(rfc3339: entry.deletedAt) else { return nil }
        self.init(
            assetID: entry.assetId,
            deletedAt: deletedAt,
            retentionUntil: entry.retentionUntil.flatMap(CapsuleTimestamp.init(rfc3339:)),
            isRestorable: entry.restorable
        )
    }
}
