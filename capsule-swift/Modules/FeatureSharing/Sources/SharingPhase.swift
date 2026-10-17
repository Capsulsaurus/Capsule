import CapsuleDomain
import CapsulePorts
import Foundation

// MARK: - SharingPhase

/// The load state every screen in this module reports.
///
/// Five cases rather than a `Bool` pair because this group of screens has to
/// distinguish conditions that look identical in a naive model and mean very
/// different things to a user: an inbox with nothing in it is *empty*, a
/// federated album whose origin is down is still *ready* (its entries render
/// from the local index — see *Federation — Robustness Against Connectivity
/// Loss*), and a read that could not run because there is no network is
/// ``offline`` rather than ``failed``, because there is nothing to report and
/// nothing to fix.
///
/// Degradation is deliberately **not** a case here. A partial federated view is
/// a fully loaded view with some origins unavailable; folding it into the phase
/// would make "some photos are from a server that is down" render as an error,
/// which is precisely the data-loss impression the federation doc forbids.
public enum SharingPhase: Sendable, Equatable {
    /// The first read has not returned yet.
    case loading
    /// Content is on screen.
    case ready
    /// The read succeeded and there is nothing to show.
    case empty
    /// The read needed the network and there is none. Recoverable by waiting.
    case offline
    /// The read failed with a code the catalog can render.
    case failed(ErrorCode)

    /// Whether a retry control is worth offering.
    ///
    /// Offline is excluded: retrying with no network reproduces the same state,
    /// and a button that cannot work is worse than none.
    public var permitsRetry: Bool {
        if case .failed = self { return true }
        return false
    }
}

public extension SharingPhase {
    /// Classify a thrown error against the connection this device last saw.
    ///
    /// Connectivity wins over the code: with no usable path, every failure is
    /// the same failure, and reporting a transport-shaped error code would send
    /// the user hunting for a problem that is "you are on a plane".
    static func resolve(_ error: Error, connection: ConnectionClass?) -> SharingPhase {
        if let connection, !connection.isUsable { return .offline }
        guard let capsule = error as? CapsuleError else {
            return .failed(.unknown(Self.unexpectedErrorKey))
        }
        return .failed(capsule.code)
    }

    /// The catalog key used when a non-``CapsuleError`` escapes a port. Carried
    /// as an ``ErrorCode/unknown`` so it is reported verbatim rather than
    /// coerced into a code that means something else.
    static let unexpectedErrorKey = "error.client.unexpected"
}

// MARK: - SharingConnectivity

/// The connection class, read through ``SyncPort``.
///
/// A separate value rather than a field on each view model because every screen
/// here needs the same answer for the same reason — to tell "no network" apart
/// from "this failed" — and because a screen constructed without one (a
/// preview, a unit test that does not care) must still work. `nil` means
/// "unknown", never "offline": guessing offline would make every port failure
/// render as a connectivity problem.
public struct SharingConnectivity: Sendable {
    private let sync: (any SyncPort)?

    public init(sync: (any SyncPort)? = nil) {
        self.sync = sync
    }

    /// The current connection class, or `nil` when it cannot be established.
    public func probe() async -> ConnectionClass? {
        guard let sync else { return nil }
        return try? await sync.status().connectionClass
    }
}
