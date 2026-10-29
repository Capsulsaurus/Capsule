import CapsuleCatalog
import CapsuleFoundation
import Foundation

/// The ``ManagedWorkspace`` of a library with no Rust core behind it: the mock
/// lane, previews and unit tests.
///
/// It keeps the same contract as the FFI lane's workspace, minus the signing.
/// An import writes the media file, a sidecar and the catalog row as one item,
/// rolled back together when a step fails. A delete opens a retention window,
/// a restore is refused once that window has closed, and a purge never skips
/// the trash and removes the files before the row. Nothing it writes syncs,
/// which is why the FFI lane does not use it.
public actor LocalManagedWorkspace: ManagedWorkspace {
    private let layout: ManagedLibraryLayout
    private let fileStore: any FileStore
    private let catalog: InMemoryAssetCatalog
    private let hasher: any ContentHasher
    private let metadataExtractor: any MediaMetadataExtracting
    private let sidecarCoder: any SidecarCoding
    private let clock: @Sendable () -> Int64
    /// The end of each trashed asset's window, Unix epoch seconds. An asset
    /// trashed with no entry here (a row seeded already deleted) has a window
    /// that never closes, as a `delete` with no readable `retention_until` does.
    private var retentionUntil: [String: Int64] = [:]

    /// - Parameter clock: Unix epoch seconds now; tests pin it to judge the
    ///   retention window deterministically.
    public init(
        layout: ManagedLibraryLayout,
        fileStore: any FileStore,
        catalog: InMemoryAssetCatalog,
        hasher: any ContentHasher = CryptoKitHasher(),
        metadataExtractor: any MediaMetadataExtracting = ImageIOMetadataExtractor(),
        sidecarCoder: any SidecarCoding = JSONSidecarCoder(),
        clock: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970) }
    ) {
        self.layout = layout
        self.fileStore = fileStore
        self.catalog = catalog
        self.hasher = hasher
        self.metadataExtractor = metadataExtractor
        self.sidecarCoder = sidecarCoder
        self.clock = clock
    }

    // MARK: Seal

    public func sealAsset(fileName: String, bytes: Data) async throws -> String {
        let hash = hasher.hash(bytes)
        let metadata = metadataExtractor.extractMetadata(from: bytes, filename: fileName)
        let uuid = UUIDv7.string()
        let now = clock()
        let captureTimestamp = metadata.captureTimestamp ?? now
        let captureDate = Date(timeIntervalSince1970: TimeInterval(captureTimestamp))
        let pathExtension = (fileName as NSString).pathExtension.lowercased()
        let fileExtension = pathExtension.isEmpty ? "jpg" : pathExtension

        let mediaURL = layout.mediaFile(uuid: uuid, fileExtension: fileExtension, captureDate: captureDate)
        let sidecarURL = layout.sidecarFile(uuid: uuid, captureDate: captureDate)

        try await fileStore.createDirectory(at: mediaURL.deletingLastPathComponent())
        try await fileStore.write(bytes, to: mediaURL)
        do {
            let sidecar = Self.sidecar(
                uuid: uuid,
                fileName: fileName,
                hash: hash,
                metadata: metadata,
                importedAt: now,
                captureTimestamp: captureTimestamp
            )
            try await fileStore.write(sidecarCoder.encode(sidecar), to: sidecarURL)
            try await catalog.insertAsset(CatalogAsset(
                id: uuid,
                assetType: metadata.assetType,
                captureTimestamp: captureTimestamp,
                importTimestamp: now,
                hashSHA256: hash,
                width: metadata.pixelWidth.map(Int64.init),
                height: metadata.pixelHeight.map(Int64.init),
                latitude: metadata.latitude,
                longitude: metadata.longitude
            ))
        } catch {
            // Roll the partial item back so a failed import leaves nothing.
            try? await fileStore.removeItem(at: mediaURL)
            try? await fileStore.removeItem(at: sidecarURL)
            CapsuleLog.managedStore.error(
                "local seal rolled back for \(fileName, privacy: .public): \(String(describing: error), privacy: .public)"
            )
            throw error
        }
        CapsuleLog.managedStore.debug("local seal of \(fileName, privacy: .public) as \(uuid, privacy: .public)")
        return uuid
    }

    // MARK: Trash

    public func deleteAsset(id: String, retainDays: UInt32) async throws {
        guard var row = await catalog.asset(id: id) else { throw TrashRefusal.notFound }
        guard !row.isDeleted else {
            CapsuleLog.managedStore.debug("local delete of \(id, privacy: .public) already in the trash; window kept")
            return
        }
        let now = clock()
        row.isDeleted = true
        row.deletedAt = now
        await catalog.upsertAsset(row)
        retentionUntil[id] = now + Int64(retainDays) * 86400
        CapsuleLog.managedStore.info("local delete of \(id, privacy: .public), retained \(retainDays) days")
    }

    public func restoreAsset(id: String) async throws {
        var row = try await trashedRow(id)
        if let until = retentionUntil[id], clock() >= until {
            CapsuleLog.managedStore.info("local restore of \(id, privacy: .public) refused: retention window closed")
            let rendered = ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: TimeInterval(until)))
            throw TrashRefusal.retentionElapsed(retentionUntil: rendered)
        }
        row.isDeleted = false
        row.deletedAt = nil
        await catalog.upsertAsset(row)
        retentionUntil[id] = nil
        CapsuleLog.managedStore.info("local restore of \(id, privacy: .public)")
    }

    /// Files first, then the row: a file that cannot be removed throws and
    /// leaves the asset in the trash for a retry, rather than reporting a
    /// photograph destroyed while its bytes are still on disk.
    public func purgeAsset(id: String) async throws -> ManagedPurgeOutcome {
        let row = try await trashedRow(id)
        let captureDate = Date(timeIntervalSince1970: TimeInterval(row.captureTimestamp))
        try await removeAssetFiles(uuid: id, captureDate: captureDate)
        await catalog.removeAsset(id: id)
        retentionUntil[id] = nil
        CapsuleLog.managedStore.info("local purge of \(id, privacy: .public)")
        return ManagedPurgeOutcome(assetID: id)
    }

    // MARK: Private

    // swiftlint:disable:next function_parameter_count
    private static func sidecar(
        uuid: String,
        fileName: String,
        hash: String,
        metadata: MediaMetadata,
        importedAt: Int64,
        captureTimestamp: Int64
    ) -> CatalogSidecar {
        CatalogSidecar(
            version: 1,
            uuid: uuid,
            assetType: metadata.assetType,
            originalFilename: fileName,
            importTimestamp: importedAt,
            modifiedTimestamp: importedAt,
            hashSHA256: hash,
            fileSize: UInt64(max(0, metadata.fileSize)),
            importerVersion: "capsule-ios/0.1.0",
            rawshiftVersion: "0.0.0",
            captureTimestamp: captureTimestamp,
            width: metadata.pixelWidth.flatMap { UInt32(exactly: $0) },
            height: metadata.pixelHeight.flatMap { UInt32(exactly: $0) },
            cameraMake: metadata.cameraMake,
            cameraModel: metadata.cameraModel,
            gpsLatitude: metadata.latitude,
            gpsLongitude: metadata.longitude
        )
    }

    private func trashedRow(_ id: String) async throws -> CatalogAsset {
        guard let row = await catalog.asset(id: id) else { throw TrashRefusal.notFound }
        guard row.isDeleted else { throw TrashRefusal.notInTrash }
        return row
    }

    /// Remove an asset's media file and paired sidecar, addressed by stem:
    /// the row does not record the extension the import derived, so
    /// `{uuid}.*` inside the capture-date partition is what both sides agree
    /// on. A file already gone is not an error, so a retried purge converges.
    private func removeAssetFiles(uuid: String, captureDate: Date) async throws {
        let directory = layout.mediaDirectory(forCaptureDate: captureDate)
        guard await fileStore.fileExists(at: directory) else { return }
        for entry in try await fileStore.contentsOfDirectory(at: directory)
            where entry.deletingPathExtension().lastPathComponent == uuid {
            try await fileStore.removeItem(at: entry)
            CapsuleLog.managedStore.debug("purged \(entry.lastPathComponent, privacy: .public)")
        }
    }
}
