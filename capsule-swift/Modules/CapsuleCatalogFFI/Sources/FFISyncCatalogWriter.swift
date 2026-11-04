import CapsuleCatalog
import CapsuleDomain
import CapsuleFoundation
import CryptoKit
import Foundation
import os
import UniformTypeIdentifiers

/// The sync catalog writer's log: INFO for each row it writes or moves into
/// or out of the trash, DEBUG for an original it keeps or discards, WARNING
/// for a timestamp it cannot read.
private let writerLog = Logger(subsystem: CapsuleLog.subsystem, category: "sync-catalog")

// MARK: - Seam

/// What one applied sync entry became in the managed store.
enum FFISyncCatalogWrite: Equatable, Sendable {
    /// The asset's row was upserted with the entry's facts. `placedOriginal`
    /// is whether the verified original was moved into the store; `false`
    /// when the store already held one for the asset, or none was handed over.
    case upserted(placedOriginal: Bool)
    /// A `delete` moved the asset's row into the trash.
    case trashed
    /// A `trash-restore` moved the asset's row out of the trash.
    case restored
    /// Nothing was written: a `delete` or `trash-restore` of an asset with no
    /// row here, an entry with no metadata whose action the store does not
    /// project, or metadata with neither an original nor a row to update.
    case unwritten
}

/// Where the sync driver writes each applied entry: the managed catalog's row
/// and the store's copy of the verified original.
protocol FFISyncCatalogWriting: Sendable {
    /// Write the applied entry's `facts`. `original`, when given, is the
    /// verified plaintext `FfiWorkspace.openSyncedOriginal` produced; it is
    /// moved into the store, or left for the caller to remove.
    func write(_ facts: FfiAssetFacts, original: URL?) async throws -> FFISyncCatalogWrite
}

// MARK: - FFISyncCatalogWriter

/// The managed catalog's side of a sync: each applied entry's verified facts
/// become the asset's `FFIAssetCatalog` row, shaped as the workspace's own
/// import projects one, and its verified original is kept in the store's
/// `media/{YYYY}/{YYYY-MM}` partition under the name the workspace gives one,
/// so the timeline and the media reader find a synced asset as they find an
/// imported one.
///
/// An entry with metadata (a `create`, a `metadata-update`, …) upserts the
/// row. The fields the entry's facts carry are overwritten; the ones they do
/// not (trash state, stack placement, location, import time) are kept from
/// the row already there, and a new row is dated as imported now. The
/// original is placed before the row is written, so a row never names an
/// original that is not on disk; where the store already holds one for the
/// asset, it is kept and the new copy is left to be removed. A `delete` moves
/// an existing row into the trash and a `trash-restore` moves it out; for an
/// asset with no row, both write nothing.
///
/// LQIP columns stay empty and the capture timezone unresolved, as the
/// workspace's own projection leaves them.
public actor FFISyncCatalogWriter: FFISyncCatalogWriting {
    /// The head manifest action that moves an asset into the trash.
    static let deleteAction = "delete"
    /// The head manifest action that moves an asset out of the trash.
    static let restoreAction = "trash-restore"
    /// The extension an original is stored under when its MIME type names none.
    static let fallbackExtension = "bin"

    private let openCatalog: @Sendable () throws -> any AssetCatalog
    private let mediaDirectory: @Sendable (Date) -> URL
    private let now: @Sendable () -> Date
    private var opened: (any AssetCatalog)?

    /// Write into the catalog at `catalogFile`, opened on the first write,
    /// keeping originals under `mediaDirectory(captureDate)`. A new row's
    /// import time is `now()`.
    public init(
        catalogFile: URL,
        mediaDirectory: @escaping @Sendable (Date) -> URL,
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.init(
            openingCatalog: {
                try FileManager.default.createDirectory(
                    at: catalogFile.deletingLastPathComponent(),
                    withIntermediateDirectories: true
                )
                return try FFIAssetCatalog(openingCatalogAt: catalogFile)
            },
            mediaDirectory: mediaDirectory,
            now: now
        )
    }

    /// Write into the catalog `openingCatalog` opens. The seam tests drive.
    init(
        openingCatalog: @escaping @Sendable () throws -> any AssetCatalog,
        mediaDirectory: @escaping @Sendable (Date) -> URL,
        now: @escaping @Sendable () -> Date
    ) {
        openCatalog = openingCatalog
        self.mediaDirectory = mediaDirectory
        self.now = now
    }

    func write(_ facts: FfiAssetFacts, original: URL?) async throws -> FFISyncCatalogWrite {
        let catalog = try openedCatalog()
        let existing = try await catalog.asset(id: facts.assetId)
        guard let metadata = facts.metadata else {
            return try await applyLifecycle(facts, to: existing, in: catalog)
        }
        let hash: String
        if let original {
            hash = try Self.sha256(of: original)
        } else if let existing {
            hash = existing.hashSHA256
        } else {
            writerLog.error("applied \(facts.assetId, privacy: .public) has neither an original nor a row; nothing written")
            return .unwritten
        }
        let row = Self.row(facts, metadata: metadata, hash: hash, existing: existing, importedAt: now())
        let placed = try original.map {
            try place($0, assetID: facts.assetId, contentType: metadata.contentType, captureSeconds: row.captureTimestamp)
        } ?? false
        try await catalog.upsertAsset(row)
        writerLog.info(
            "synced row \(facts.assetId, privacy: .public) upserted: action=\(facts.action, privacy: .public) new=\(existing == nil, privacy: .public) placed_original=\(placed, privacy: .public)"
        )
        return .upserted(placedOriginal: placed)
    }

    // MARK: Rows

    /// The row an entry with metadata projects, over the row already there.
    static func row(
        _ facts: FfiAssetFacts,
        metadata: FfiAssetMetadata,
        hash: String,
        existing: CatalogAsset?,
        importedAt: Date
    ) -> CatalogAsset {
        let capture = seconds(metadata.captureTimestamp, field: "capture_timestamp", assetID: facts.assetId)
        var row = existing ?? CatalogAsset(
            id: facts.assetId,
            assetType: assetType(metadata.contentType),
            captureTimestamp: capture,
            importTimestamp: Int64(importedAt.timeIntervalSince1970),
            hashSHA256: hash
        )
        row.assetType = assetType(metadata.contentType)
        row.captureTimestamp = capture
        row.captureUTC = capture
        row.captureTimezoneSource = nil
        row.hashSHA256 = hash
        row.width = metadata.width.map(Int64.init)
        row.height = metadata.height.map(Int64.init)
        row.albumID = facts.albumId
        row.rating = Int64(metadata.rating ?? 0)
        row.isHidden = metadata.hidden
        row.caption = metadata.caption.flatMap { $0.isEmpty ? nil : $0 }
        return row
    }

    /// The catalog's asset type for a MIME type, as the workspace derives it.
    static func assetType(_ contentType: String) -> String {
        contentType.hasPrefix("video/") ? "video" : "photo"
    }

    /// Epoch seconds of an RFC 3339 timestamp, or the epoch, logged, for one
    /// that does not parse: the workspace indexes an unreadable one the same.
    static func seconds(_ rfc3339: String, field: StaticString, assetID: String) -> Int64 {
        if let parsed = CapsuleTimestamp(rfc3339: rfc3339) { return parsed.epochSeconds }
        writerLog.warning(
            "unreadable \(String(describing: field), privacy: .public) on \(assetID, privacy: .public); indexed as the epoch"
        )
        return 0
    }

    /// Move an existing row into or out of the trash, as the entry's action
    /// says.
    private func applyLifecycle(
        _ facts: FfiAssetFacts,
        to existing: CatalogAsset?,
        in catalog: any AssetCatalog
    ) async throws -> FFISyncCatalogWrite {
        let trash: Bool
        switch facts.action {
        case Self.deleteAction: trash = true
        case Self.restoreAction: trash = false
        default:
            writerLog.info("applied \(facts.action, privacy: .public) of \(facts.assetId, privacy: .public) carries no metadata; nothing written")
            return .unwritten
        }
        guard var row = existing else {
            writerLog.info("applied \(facts.action, privacy: .public) of \(facts.assetId, privacy: .public), which has no row here; nothing written")
            return .unwritten
        }
        row.isDeleted = trash
        row.deletedAt = trash ? Self.seconds(facts.timestamp, field: "timestamp", assetID: facts.assetId) : nil
        try await catalog.upsertAsset(row)
        writerLog.info("synced row \(facts.assetId, privacy: .public) \(trash ? "trashed" : "restored", privacy: .public)")
        return trash ? .trashed : .restored
    }

    private func openedCatalog() throws -> any AssetCatalog {
        if let opened { return opened }
        let catalog = try openCatalog()
        opened = catalog
        return catalog
    }

    // MARK: Originals

    /// Move the verified plaintext at `original` into the asset's partition
    /// as `{id}.{ext}`, the workspace's naming. `false`, leaving `original`
    /// where it is, when the partition already holds the asset's original.
    private func place(_ original: URL, assetID: String, contentType: String, captureSeconds: Int64) throws -> Bool {
        let directory = mediaDirectory(Date(timeIntervalSince1970: TimeInterval(captureSeconds)))
        let fileManager = FileManager.default
        let stem = Self.fileStem(assetID)
        if fileManager.fileExists(atPath: directory.path) {
            let held = try fileManager.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).first {
                Self.fileStem($0.deletingPathExtension().lastPathComponent) == stem
                    && $0.pathExtension.lowercased() != "cbor"
            }
            if let held {
                writerLog.debug("store already holds \(held.lastPathComponent, privacy: .public); keeping it")
                return false
            }
        }
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        let destination = directory.appending(path: stem).appendingPathExtension(Self.fileExtension(contentType))
        try fileManager.moveItem(at: original, to: destination)
        writerLog.debug("synced original placed at \(destination.lastPathComponent, privacy: .public)")
        return true
    }

    /// An asset id in the form the workspace names its files with: lowercase,
    /// hyphens removed.
    static func fileStem(_ id: String) -> String {
        id.lowercased().replacingOccurrences(of: "-", with: "")
    }

    /// The extension a MIME type's preferred file type takes, lowercased.
    static func fileExtension(_ contentType: String) -> String {
        UTType(mimeType: contentType)?.preferredFilenameExtension?.lowercased() ?? fallbackExtension
    }

    /// Bytes read per chunk while hashing an original (1 MiB).
    private static let hashChunk = 1 << 20

    /// The lowercase-hex SHA-256 of the file at `url`, read in bounded chunks:
    /// the plaintext digest the signed sidecar commits to, which the
    /// workspace's own rows carry.
    static func sha256(of url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: hashChunk), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}
