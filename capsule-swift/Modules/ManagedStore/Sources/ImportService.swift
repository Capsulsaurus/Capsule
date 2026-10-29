import CapsuleCatalog
import CapsuleFoundation
import Foundation

/// Imports media files into the Capsule-managed library.
///
/// Each file runs the pipeline — read, **hash** (SHA-256), **dedup** against
/// the catalog, then **seal** through the library's ``ManagedWorkspace``. The
/// workspace owns the commit: on the FFI lane it is `FfiWorkspace.sealAsset`,
/// which derives the metadata, signs the sidecar and manifest, and writes the
/// index row the catalog then reads; a failed seal writes nothing.
///
/// All collaborators are injected behind protocols, so the pipeline is tested
/// against in-memory mocks — including a deliberately failing file store to
/// exercise rollback.
public actor ImportService {
    private let library: ManagedLibrary
    private let fileStore: any FileStore
    private let hasher: any ContentHasher

    /// - Parameter hasher: the dedup hash, which must be the SHA-256 the
    ///   workspace records on the row (`CatalogAsset.hashSHA256`), or a second
    ///   import of the same bytes is not recognised.
    public init(
        library: ManagedLibrary,
        fileStore: any FileStore,
        hasher: any ContentHasher
    ) {
        self.library = library
        self.fileStore = fileStore
        self.hasher = hasher
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

        let uuid = try await library.workspace.sealAsset(fileName: Self.sealFileName(for: source), bytes: data)
        CapsuleLog.managedStore.debug("imported \(source.originalFilename, privacy: .public) as \(uuid, privacy: .public)")
        return .imported(uuid)
    }

    /// The name the workspace reads the extension from: the original's name,
    /// or — when that has no extension — the name with the source file's
    /// extension appended, so a picker copy named `IMG_0001` still seals as the
    /// `.heic` it is.
    static func sealFileName(for source: ImportSource) -> String {
        let name = source.originalFilename
        let sourceExtension = source.url.pathExtension
        guard (name as NSString).pathExtension.isEmpty, !sourceExtension.isEmpty else { return name }
        return "\(name).\(sourceExtension)"
    }
}
