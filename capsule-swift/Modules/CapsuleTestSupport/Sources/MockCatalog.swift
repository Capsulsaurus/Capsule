import CapsuleCatalog

/// The in-memory catalog, under its historical test-facing name.
///
/// The implementation moved into `CapsuleCatalog` as ``InMemoryAssetCatalog``
/// when the catalog module was split from its Rust-backed half: the mock-lane
/// app and SwiftUI previews need it too, not just tests. This alias keeps the
/// existing test suites reading naturally.
public typealias MockCatalog = InMemoryAssetCatalog
