import CapsuleCatalog
import Foundation

// Maps the generated uniffi error onto the FFI-free `CatalogError` that every
// consumer above this boundary names. Keeping the mapping here is what lets
// `CapsuleCatalog` compile with no Rust core linked.
//
// Naming note: uniffi's generated glue is compiled *into this module*, so its
// `CatalogError` shadows the imported one. These two aliases disambiguate; use
// them rather than the bare name anywhere both are in scope.
typealias GeneratedCatalogError = CatalogError
typealias NativeCatalogError = CapsuleCatalog.CatalogError

/// Translate any error thrown across the uniffi boundary into the native enum.
///
/// The generated enum's cases map one-to-one; anything else (a uniffi internal
/// error, a panic surfaced as an error) becomes `.database` with the underlying
/// description preserved, so a failure is never swallowed.
func nativeCatalogError(_ error: Error) -> NativeCatalogError {
    guard let generated = error as? GeneratedCatalogError else {
        return .database(message: String(describing: error))
    }
    switch generated {
    case let .Database(message): return .database(message: message)
    case let .Sidecar(message): return .sidecar(message: message)
    case let .InvalidArgument(message): return .invalidArgument(message: message)
    case .ViewLocked: return .viewLocked
    }
}
