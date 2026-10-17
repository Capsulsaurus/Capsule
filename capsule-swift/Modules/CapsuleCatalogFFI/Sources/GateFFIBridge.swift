import CapsuleCatalog
import Foundation

// Maps the SR1 gate's generated uniffi types onto the FFI-free ones every
// consumer above this boundary names — the same boundary discipline
// `CatalogErrorFFIBridge` applies to `CatalogError`.
//
// Naming note: uniffi's generated glue is compiled *into this module*, so its
// `GatedView` / `LocalAuthError` / `LocalAuthGate` shadow the imported ones.
// These aliases disambiguate; use them rather than the bare names.
typealias GeneratedGatedView = GatedView
typealias GeneratedLocalAuthError = LocalAuthError
typealias GeneratedLocalAuthGate = LocalAuthGate
typealias NativeGatedView = CapsuleCatalog.GatedView
typealias NativeLocalAuthError = CapsuleCatalog.LocalAuthError
typealias NativeLocalAuthGate = CapsuleCatalog.LocalAuthGate

extension NativeGatedView {
    /// The generated enum this view crosses the boundary as.
    var ffiValue: GeneratedGatedView {
        switch self {
        case .recentlyDeleted: .recentlyDeleted
        case .hidden: .hidden
        }
    }
}

/// Translate any error thrown by a foreign gate back into the native enum.
///
/// Anything unrecognised is `.failed`: a grant is minted only on an explicit
/// success, so an error whose shape we cannot read is a refusal.
func nativeLocalAuthError(_ error: Error) -> NativeLocalAuthError {
    guard let generated = error as? GeneratedLocalAuthError else { return .failed }
    switch generated {
    case .Cancelled: return .cancelled
    case .Unavailable: return .unavailable
    case .Failed: return .failed
    }
}

/// Presents a native ``CapsuleCatalog/LocalAuthGate`` to the Rust core across
/// the uniffi foreign-trait seam.
///
/// The core drives the challenge, so the adapter runs on whatever thread Rust
/// calls it from and must be synchronous — which is why the native protocol is
/// synchronous too.
final class ForeignAuthGate: GeneratedLocalAuthGate {
    private let gate: any NativeLocalAuthGate

    init(_ gate: any NativeLocalAuthGate) {
        self.gate = gate
    }

    func authenticate(view: GeneratedGatedView) throws {
        do {
            try gate.authenticate(view: view.nativeValue)
        } catch let error as NativeLocalAuthError {
            throw error.ffiValue
        }
    }
}

private extension GeneratedGatedView {
    var nativeValue: NativeGatedView {
        switch self {
        case .recentlyDeleted: .recentlyDeleted
        case .hidden: .hidden
        }
    }
}

private extension NativeLocalAuthError {
    /// The generated error the core expects back from a refused challenge.
    var ffiValue: GeneratedLocalAuthError {
        switch self {
        case .cancelled: .Cancelled
        case .unavailable: .Unavailable
        case .failed: .Failed
        }
    }
}
