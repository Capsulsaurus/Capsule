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
}
