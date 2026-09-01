import CapsuleCatalog
import Foundation

/// The production ``CatalogOpening`` — opens the real SQLite catalog through
/// the Rust core.
///
/// Passing this into `ManagedLibrary` is what turns the mock lane's app into
/// the FFI-backed one; it is the single wiring point where the Rust core enters
/// the object graph.
public struct FFICatalogOpener: CatalogOpening {
    public init() {}

    public func openCatalog(at url: URL) throws -> any AssetCatalog {
        try FFIAssetCatalog(openingCatalogAt: url)
    }
}
