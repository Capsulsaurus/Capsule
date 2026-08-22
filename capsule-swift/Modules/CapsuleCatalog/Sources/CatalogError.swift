import Foundation

/// The catalog boundary's error type.
///
/// A Swift-native mirror of the Rust `CatalogError` uniffi enum. It is declared
/// here — in the FFI-free half of the catalog module — so that every consumer
/// (`ManagedStore`, `AssetKit`, the mock adapters, the feature view models) can
/// name catalog failures without linking the Rust core. `CapsuleCatalogFFI` maps
/// the generated enum onto these cases at its boundary; nothing above that
/// boundary ever sees a generated type.
///
/// The case set is a **parity contract** with `capsule-core-ffi`'s
/// `CatalogError`: adding a variant there requires adding it here.
public enum CatalogError: Error, Sendable, Equatable {
    /// The underlying store rejected the operation.
    case database(message: String)
    /// A sidecar could not be encoded or decoded.
    case sidecar(message: String)
    /// A caller passed a value the catalog cannot accept.
    case invalidArgument(message: String)

    /// The human-readable detail, which — per the i18n contract — stays English;
    /// clients localize the high-level message from the catalog key instead.
    public var message: String {
        switch self {
        case let .database(message), let .sidecar(message), let .invalidArgument(message):
            message
        }
    }
}

extension CatalogError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case let .database(message): "database error: \(message)"
        case let .sidecar(message): "sidecar error: \(message)"
        case let .invalidArgument(message): "invalid argument: \(message)"
        }
    }
}
