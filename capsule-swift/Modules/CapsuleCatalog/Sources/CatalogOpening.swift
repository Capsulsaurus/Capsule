import Foundation

/// Opens the catalog that backs a managed library.
///
/// This is the seam that keeps `ManagedStore` — and everything above it —
/// free of any dependency on the Rust core. `CapsuleCatalogFFI` provides the
/// production opener over `FFIAssetCatalog`; the mock lane hands
/// `ManagedLibrary` an in-memory catalog directly, shared with the workspace
/// that writes it. Neither module needs to know the other exists.
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
