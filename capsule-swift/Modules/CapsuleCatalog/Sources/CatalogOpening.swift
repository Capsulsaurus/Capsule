import Foundation

/// Opens the catalog that backs a managed library.
///
/// This is the seam that keeps `ManagedStore` — and everything above it —
/// free of any dependency on the Rust core. `CapsuleCatalogFFI` provides the
/// production opener over `FFIAssetCatalog`; the mock lane supplies one over an
/// in-memory catalog. Neither module needs to know the other exists.
public protocol CatalogOpening: Sendable {
    /// Open (creating and migrating if necessary) the catalog stored at `url`.
    ///
    /// - Throws: ``CatalogError`` if the catalog cannot be opened or migrated.
    func openCatalog(at url: URL) throws -> any AssetCatalog
}

/// A ``CatalogOpening`` that hands back a catalog that is already open.
///
/// Used by the convenience initializer that takes a live catalog directly, so
/// there is exactly one code path through ``CatalogOpening`` rather than a
/// special case.
public struct PreopenedCatalogOpener: CatalogOpening {
    private let catalog: any AssetCatalog

    public init(catalog: any AssetCatalog) {
        self.catalog = catalog
    }

    public func openCatalog(at _: URL) throws -> any AssetCatalog {
        catalog
    }
}

/// A ``CatalogOpening`` that opens a fresh in-memory catalog, ignoring the URL.
///
/// This is the mock lane's opener: the app runs its whole managed-library path
/// — directory skeleton, import, timeline queries — with nothing on disk and no
/// Rust core. State lives for the process lifetime, which is exactly what a
/// scenario-driven mock wants.
public struct InMemoryCatalogOpener: CatalogOpening {
    private let schemaVersion: UInt32

    public init(schemaVersion: UInt32 = 2) {
        self.schemaVersion = schemaVersion
    }

    public func openCatalog(at _: URL) throws -> any AssetCatalog {
        InMemoryAssetCatalog(schemaVersion: schemaVersion)
    }
}
