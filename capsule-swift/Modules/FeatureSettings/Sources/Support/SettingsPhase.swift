import CapsuleDomain
import CapsulePorts
import Foundation

// MARK: - SettingsPhase

/// What a settings screen is showing right now.
///
/// Every screen in this module renders one of these five, which is why it is a
/// closed enum rather than a pair of `isLoading` / `error` properties: those
/// two admit states that cannot be drawn (loading *and* failed), and they make
/// "empty" indistinguishable from "not loaded yet". A settings screen that
/// cannot tell a user the difference between "you have no enrolled devices" and
/// "we could not reach your server" is lying by omission.
public enum SettingsPhase: Sendable, Equatable {
    /// The first read has not returned.
    case loading
    /// Content is available and drawn.
    case ready
    /// The read succeeded and there is genuinely nothing to show.
    case empty
    /// The device has no usable connection, so the read could not have
    /// succeeded. Distinct from ``failed(_:)`` because the user's action is
    /// different: wait, rather than report.
    case offline
    /// The read failed for a reason the taxonomy names. The code is the catalog
    /// key its user-facing message is looked up by.
    case failed(ErrorCode)

    /// Whether a progress indicator should be on screen.
    public var isLoading: Bool { self == .loading }

    /// Whether the screen has content to draw.
    public var isReady: Bool { self == .ready }
}

// MARK: - SettingsConnectivity

/// The connectivity probe every settings view model uses to tell "offline"
/// apart from "failed".
///
/// Connectivity is deliberately **not** an error code: the taxonomy in
/// ``ErrorCode`` names server-side conditions, and no server answers when the
/// radio is off. The honest signal is the connection class ``SyncPort`` already
/// tracks, and reading it is a local call that answers even when nothing else
/// does — which is exactly the property an offline-state resolver needs.
///
/// A value type wrapping one port rather than a shared observable object: a
/// view model that holds it can be constructed in a test with a stub sync port
/// and nothing else.
public struct SettingsConnectivity: Sendable {
    private let sync: any SyncPort

    public init(sync: any SyncPort) {
        self.sync = sync
    }

    /// The connection class right now, or `nil` when even the local read failed
    /// — in which case the caller must not claim the user is offline.
    public func connectionClass() async -> ConnectionClass? {
        try? await sync.status().connectionClass
    }

    /// Whether the device is definitely without a usable connection.
    ///
    /// `false` when the class cannot be read at all. Guessing "offline" from an
    /// unreadable state would blame the network for a bug.
    ///
    /// Deliberately `== .offline` rather than `!isUsable`. `isUsable` is also
    /// false for an **unknown** class — one written by a newer client — and an
    /// unknown class is the same epistemic position as an unreadable one: we do
    /// not know. Treating it as offline makes ``phase(for:)`` return `.offline`
    /// and discard the real error code, so a user on a working connection would
    /// be told to check their network while the actual failure went unreported.
    public func isOffline() async -> Bool {
        guard let connection = await connectionClass() else { return false }
        return connection == .offline
    }

    /// Classify a thrown error into the phase a screen should render.
    ///
    /// Offline wins over the code: a request that failed while the radio was
    /// off failed *because* the radio was off, whatever code the transport
    /// invented on the way out.
    public func phase(for error: any Error) async -> SettingsPhase {
        if await isOffline() { return .offline }
        guard let capsuleError = error as? CapsuleError else {
            return .failed(.unknown(SettingsPhase.unclassifiedErrorKey))
        }
        return .failed(capsuleError.code)
    }
}

public extension SettingsPhase {
    /// The catalog key used when a screen catches something that is not a
    /// ``CapsuleError``. Carried verbatim rather than coerced onto a real code,
    /// so a lookup miss is visible instead of a plausible wrong message.
    static let unclassifiedErrorKey = "error.client.unclassified"
}
