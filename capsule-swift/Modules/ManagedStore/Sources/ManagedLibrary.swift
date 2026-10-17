import CapsuleCatalog
import CapsuleFoundation
import Foundation

/// The Capsule-managed, on-disk photo library.
///
/// Owns the library's location (``ManagedLibraryLayout``) and lazily opens the
/// catalog, ensuring the directory skeleton exists first. The import pipeline
/// and the managed provider share one instance so they read and write a single
/// catalog.
///
/// *How* the catalog is opened is injected as ``CatalogOpening`` rather than
/// hard-wired: a build that links the Rust core passes an opener over
/// `FFIAssetCatalog`, while the mock lane, previews, and unit tests pass an
/// in-memory one. This module therefore compiles with no Rust core present.
public actor ManagedLibrary {
    /// The library's on-disk layout.
    public let layout: ManagedLibraryLayout

    private let fileStore: any FileStore
    private let catalogOpener: any CatalogOpening
    private var openedCatalog: (any AssetCatalog)?

    public init(
        layout: ManagedLibraryLayout,
        fileStore: any FileStore = SystemFileStore(),
        catalogOpener: any CatalogOpening
    ) {
        self.layout = layout
        self.fileStore = fileStore
        self.catalogOpener = catalogOpener
    }

    /// Create a library over an already-open catalog, skipping the lazy open —
    /// for tests and previews.
    public init(
        layout: ManagedLibraryLayout,
        fileStore: any FileStore,
        catalog: any AssetCatalog
    ) {
        self.layout = layout
        self.fileStore = fileStore
        catalogOpener = PreopenedCatalogOpener(catalog: catalog)
        openedCatalog = catalog
    }

    /// The catalog, opening it (and creating the directory skeleton) on first
    /// use. Subsequent calls return the already-open catalog.
    public func catalog() async throws -> any AssetCatalog {
        if let openedCatalog {
            return openedCatalog
        }
        // Bound to a local first: the os_log interpolation is an autoclosure, so
        // reading `layout` inline would require an explicit `self.` capture that
        // the formatter strips back out.
        let rootPath = layout.root.path
        CapsuleLog.managedStore.info("preparing managed library at \(rootPath, privacy: .public)")
        for directory in layout.skeletonDirectories {
            try await fileStore.createDirectory(at: directory)
        }
        let catalog = try catalogOpener.openCatalog(at: layout.catalogFile)
        openedCatalog = catalog
        CapsuleLog.managedStore.info("managed library catalog opened")
        return catalog
    }

    /// Remove an asset's bytes — its media file and paired sidecar — from the store.
    ///
    /// Addressed by *stem* rather than by an exact path because the catalog does
    /// not record a file extension: ``ImportService`` derives one from the source
    /// filename, so `{uuid}.*` inside the capture-date partition is the only thing
    /// both sides agree on. That also sweeps the `.cbor` sidecar without naming it.
    ///
    /// A missing file is not an error. A purge that runs twice, or after a partial
    /// import, must still converge on "the bytes are gone".
    ///
    /// - Parameter captureDate: the asset's capture instant, which is what decides
    ///   its `media/{YYYY}/{YYYY-MM}` partition. Read it from the catalog row
    ///   *before* deleting that row, or the directory is no longer derivable.
    public func removeAssetFiles(uuid: String, captureDate: Date) async throws {
        let directory = layout.mediaDirectory(forCaptureDate: captureDate)
        guard await fileStore.fileExists(at: directory) else { return }
        for entry in try await fileStore.contentsOfDirectory(at: directory)
            where entry.deletingPathExtension().lastPathComponent == uuid {
            try await fileStore.removeItem(at: entry)
            CapsuleLog.managedStore.debug("purged \(entry.lastPathComponent, privacy: .public)")
        }
    }
}
