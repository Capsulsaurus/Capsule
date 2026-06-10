import CapsuleCatalog
import CapsuleFoundation
import Foundation

/// The Capsule-managed, on-disk photo library.
///
/// Owns the library's location (``ManagedLibraryLayout``) and lazily opens the
/// SQLite catalog through the Rust core, ensuring the directory skeleton
/// exists first. The import pipeline and the managed provider share one
/// instance so they read and write a single catalog.
public actor ManagedLibrary {
    /// The library's on-disk layout.
    public let layout: ManagedLibraryLayout

    private let fileStore: any FileStore
    private var openedCatalog: (any AssetCatalog)?

    public init(layout: ManagedLibraryLayout, fileStore: any FileStore = SystemFileStore()) {
        self.layout = layout
        self.fileStore = fileStore
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
        openedCatalog = catalog
    }

    /// The catalog, opening it (and creating the directory skeleton) on first
    /// use. Subsequent calls return the already-open catalog.
    public func catalog() async throws -> any AssetCatalog {
        if let openedCatalog {
            return openedCatalog
        }
        CapsuleLog.managedStore.info("preparing managed library at \(self.layout.root.path, privacy: .public)")
        for directory in layout.skeletonDirectories {
            try await fileStore.createDirectory(at: directory)
        }
        let catalog = try CapsuleCatalog(openingCatalogAt: layout.catalogFile)
        openedCatalog = catalog
        CapsuleLog.managedStore.info("managed library catalog opened")
        return catalog
    }
}
