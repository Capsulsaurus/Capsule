import Foundation

// MARK: - GatedView

/// A view that requires fresh local authentication before it opens (*Local
/// Gallery — SR1*). Grants are **per-view**: authenticating for one never opens
/// the other.
///
/// A Swift-native mirror of the Rust `GatedView` uniffi enum, declared here — in
/// the FFI-free half of the catalog module — so `AssetCatalog` can name the
/// gated reads without linking the Rust core. `CapsuleCatalogFFI` maps this onto
/// the generated enum at its boundary.
///
/// The case set is a **parity contract** with `capsule-core-ffi`'s `GatedView`:
/// adding a variant there requires adding it here.
public enum GatedView: Sendable, Hashable, CaseIterable {
    /// The trash / "Recently Deleted" listing of soft-deleted assets.
    case recentlyDeleted
    /// The user-hidden set (assets whose sidecar `hidden` register is set).
    case hidden
}

// MARK: - LocalAuthError

/// A refusal surfaced by a ``LocalAuthGate`` implementation.
///
/// The native mirror of `capsule-core-ffi`'s `LocalAuthError`, and a parity
/// contract with it in the same way ``CatalogError`` is.
public enum LocalAuthError: Error, Sendable, Equatable {
    /// The user dismissed or cancelled the authentication prompt. A cancel is
    /// not a failure and must not be reported as one.
    case cancelled
    /// No local authentication method is available — no biometric enrolled and
    /// no device credential set — so the platform cannot challenge.
    case unavailable
    /// The challenge was presented and refused (wrong credential, failed
    /// biometric).
    case failed
}

extension LocalAuthError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .cancelled: "local authentication was cancelled"
        case .unavailable: "no local authentication method is available"
        case .failed: "local authentication failed"
        }
    }
}

// MARK: - LocalAuthGate

/// The per-platform local-authentication ceremony, as the catalog's gate sees
/// it: one synchronous challenge, and only its outcome.
///
/// The **biometric → credential fallback** is entirely the implementation's
/// concern; an adapter tries the enrolled biometric first and falls back to the
/// device credential, surfacing only the outcome here. Any successful return
/// counts as a fresh authentication regardless of which method produced it.
///
/// - Important: `authenticate(view:)` is **synchronous** because the Rust core
///   drives it across a uniffi foreign-trait seam that has no `await` to spend —
///   so it blocks its calling thread for as long as the prompt is on screen.
///   Call it only through an ``AssetCatalog``, whose actor isolation keeps that
///   block off the main thread.
public protocol LocalAuthGate: Sendable {
    /// Perform a fresh local-authentication challenge for `view`, throwing a
    /// ``LocalAuthError`` when the platform refuses.
    func authenticate(view: GatedView) throws
}
