import Foundation

// MARK: - ScreenState

/// The four states every screen in this module must be able to render, plus the
/// idle state before anything has been asked for.
///
/// Modelled as one closed value rather than a pile of booleans because
/// `isLoading && error != nil` is a state no screen has copy for, and a lint
/// rule cannot catch it. A view switches on this and the compiler proves every
/// case is handled.
///
/// "Offline" is not a fifth case: an unreachable server arrives as a *failure*
/// whose ``AuthPresentableError/kind`` is
/// ``AuthErrorKind/temporarilyUnavailable``, so there is exactly one place a
/// screen decides what unreachable looks like.
public enum ScreenState: Sendable, Equatable {
    /// Nothing requested yet.
    case idle
    /// Work in flight.
    case loading
    /// Loaded, with something to show.
    case ready
    /// Loaded, with nothing to show. Distinct from ``ready`` because "no
    /// sessions" and "sessions not loaded" need different copy.
    case empty
    /// Failed. Carries the presentable classification, never a raw error.
    case failed(AuthPresentableError)

    public var isLoading: Bool { self == .loading }

    public var failure: AuthPresentableError? {
        guard case let .failed(error) = self else { return nil }
        return error
    }

    /// Whether this state is the "cannot reach the server" presentation.
    public var isOffline: Bool {
        failure?.isOffline ?? false
    }
}
