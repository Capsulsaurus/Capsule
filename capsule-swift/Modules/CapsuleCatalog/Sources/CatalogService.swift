import Foundation

/// A Swift actor over the Rust `Catalog` UniFFI object.
///
/// All SQLite access is confined to this actor, off the main thread. The raw
/// UniFFI types (`Catalog`, `AssetRecord`, …) never escape this module — Phase 1
/// gives callers a Swift-native, `Sendable` async API in their place.
///
/// This Phase 0 skeleton exists to verify the Rust ↔ Swift bridge links and
/// runs; see ``ffiSchemaVersionSmokeCheck()``.
public actor CatalogService {
    public init() {}

    /// Opens an ephemeral in-memory catalog and returns its schema version.
    ///
    /// A smoke test of the FFI bridge: exercising it proves the generated
    /// bindings, the `CapsuleCoreFFI` xcframework, and the bundled SQLite all
    /// link and execute from Swift.
    public static func ffiSchemaVersionSmokeCheck() throws -> UInt32 {
        let catalog = try Catalog.openInMemory()
        return try catalog.schemaVersion()
    }
}
