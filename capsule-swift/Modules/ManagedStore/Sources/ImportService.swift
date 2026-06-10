import CapsuleCatalog
import CapsuleFoundation
import Foundation

/// Imports media files into the Capsule-managed library.
///
/// Each file runs the pipeline — read, **hash** (SHA-256), **dedup** against
/// the catalog, extract metadata, then a per-item commit: write the media
/// file, write its CBOR sidecar, insert the catalog row. If the sidecar or
/// catalog step fails the media file and sidecar are removed, so a failed
/// import leaves nothing partial behind.
///
/// All collaborators are injected behind protocols, so the pipeline is tested
/// against in-memory mocks — including a deliberately failing file store to
/// exercise rollback.
public actor ImportService {
    private let library: ManagedLibrary
    private let fileStore: any FileStore
    private let hasher: any ContentHasher
    private let metadataExtractor: any MediaMetadataExtracting

    public init(
        library: ManagedLibrary,
        fileStore: any FileStore,
        hasher: any ContentHasher,
        metadataExtractor: any MediaMetadataExtracting
    ) {
        self.library = library
        self.fileStore = fileStore
        self.hasher = hasher
        self.metadataExtractor = metadataExtractor
    }

    /// Import every source, returning a per-file account of the outcome.
    public func importAssets(from sources: [ImportSource]) async -> ImportResult {
        let signposter = CapsuleSignpost.importPipeline
        let interval = signposter.beginInterval("import")
        defer { signposter.endInterval("import", interval) }

        let catalog: any AssetCatalog
        do {
            catalog = try await library.catalog()
        } catch {
            CapsuleLog.managedStore.error("import aborted, catalog unavailable: \(String(describing: error), privacy: .public)")
            Diagnostics.shared.recordError(operation: .importRun)
            return ImportResult(failures: sources.map {
                ImportFailure(filename: $0.originalFilename, reason: "Library unavailable.")
            })
        }

        var result = ImportResult()
        for source in sources {
            do {
                switch try await importOne(source, catalog: catalog) {
                case let .imported(uuid):
                    result.importedAssetIDs.append(uuid)
                case .duplicate:
                    result.duplicateFilenames.append(source.originalFilename)
                }
            } catch {
                CapsuleLog.managedStore.error("import failed for \(source.originalFilename, privacy: .public): \(String(describing: error), privacy: .public)")
                result.failures.append(ImportFailure(
                    filename: source.originalFilename,
                    reason: String(describing: error)
                ))
            }
        }
        CapsuleLog.managedStore.info("import finished: \(result.importedCount) imported, \(result.duplicateCount) duplicate, \(result.failureCount) failed")
        return result
    }

    // MARK: Private

    private enum ItemOutcome {
        case imported(String)
        case duplicate
    }

    private func importOne(_ source: ImportSource, catalog: any AssetCatalog) async throws -> ItemOutcome {
        let data = try await fileStore.read(at: source.url)
        let hash = hasher.hash(data)

        if try await catalog.asset(hashSHA256: hash) != nil {
            CapsuleLog.managedStore.debug("skipping duplicate: \(source.originalFilename, privacy: .public)")
            return .duplicate
        }

        let metadata = metadataExtractor.extractMetadata(from: data, filename: source.originalFilename)
        let uuid = UUIDv7.string()
        let captureTimestamp = metadata.captureTimestamp ?? Int64(Date().timeIntervalSince1970)
        let captureDate = Date(timeIntervalSince1970: TimeInterval(captureTimestamp))
        let fileExtension = source.url.pathExtension.isEmpty
            ? "jpg"
            : source.url.pathExtension.lowercased()

        let layout = library.layout
        let mediaURL = layout.mediaFile(uuid: uuid, fileExtension: fileExtension, captureDate: captureDate)
        let sidecarURL = layout.sidecarFile(uuid: uuid, captureDate: captureDate)

        try await fileStore.createDirectory(at: mediaURL.deletingLastPathComponent())
        try await fileStore.write(data, to: mediaURL)

        do {
            let sidecar = makeSidecar(
                uuid: uuid,
                hash: hash,
                metadata: metadata,
                source: source,
                captureTimestamp: captureTimestamp
            )
            try await fileStore.write(SidecarCodec.encode(sidecar), to: sidecarURL)
            try await catalog.insertAsset(makeCatalogAsset(
                uuid: uuid,
                hash: hash,
                metadata: metadata,
                captureTimestamp: captureTimestamp
            ))
        } catch {
            // Roll the partial item back so a failed import leaves nothing.
            try? await fileStore.removeItem(at: mediaURL)
            try? await fileStore.removeItem(at: sidecarURL)
            throw error
        }

        CapsuleLog.managedStore.debug("imported \(source.originalFilename, privacy: .public) as \(uuid, privacy: .public)")
        return .imported(uuid)
    }

    private func makeSidecar(
        uuid: String,
        hash: String,
        metadata: MediaMetadata,
        source: ImportSource,
        captureTimestamp: Int64
    ) -> CatalogSidecar {
        let now = Int64(Date().timeIntervalSince1970)
        return CatalogSidecar(
            version: 1,
            uuid: uuid,
            assetType: metadata.assetType,
            originalFilename: source.originalFilename,
            importTimestamp: now,
            modifiedTimestamp: now,
            hashSHA256: hash,
            fileSize: UInt64(max(0, metadata.fileSize)),
            importerVersion: "capsule-ios/0.1.0",
            rawshiftVersion: "0.0.0",
            captureTimestamp: captureTimestamp,
            width: metadata.pixelWidth.flatMap { UInt32(exactly: $0) },
            height: metadata.pixelHeight.flatMap { UInt32(exactly: $0) },
            cameraMake: metadata.cameraMake,
            cameraModel: metadata.cameraModel
        )
    }

    private func makeCatalogAsset(
        uuid: String,
        hash: String,
        metadata: MediaMetadata,
        captureTimestamp: Int64
    ) -> CatalogAsset {
        CatalogAsset(
            id: uuid,
            assetType: metadata.assetType,
            captureTimestamp: captureTimestamp,
            importTimestamp: Int64(Date().timeIntervalSince1970),
            hashSHA256: hash,
            width: metadata.pixelWidth.map(Int64.init),
            height: metadata.pixelHeight.map(Int64.init)
        )
    }
}
