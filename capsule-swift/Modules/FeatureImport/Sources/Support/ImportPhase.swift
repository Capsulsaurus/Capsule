import CapsuleDomain
import CapsulePorts
import Foundation

// MARK: - ImportPhase

/// What an import screen is showing right now.
///
/// Offline is a phase of its own rather than a flavour of ``failed(_:)`` because
/// the user's next move differs: an offline scan still works — the source is on
/// this device — while an offline *plan* cannot check the library's duplicates
/// against the server. Telling a user their import is broken when their network
/// merely is would send them to the wrong remedy.
public enum ImportPhase: Sendable, Equatable {
    /// The first read has not returned.
    case loading
    /// Content is available and the screen is interactive.
    case ready
    /// The read succeeded and there is genuinely nothing to show.
    case empty
    /// No usable connection.
    case offline
    /// The read failed for a reason the taxonomy names. The code is the catalog
    /// key its user-facing message is looked up by.
    case failed(ErrorCode)

    /// Whether the screen has content to draw.
    public var isReady: Bool { self == .ready }
}

// MARK: - ImportConnectivity

/// The probe that tells "offline" apart from "failed".
///
/// Connectivity is deliberately not an error code: the taxonomy names
/// server-side conditions, and no server answers when the radio is off. Reading
/// the connection class ``SyncPort`` already tracks is a local call that answers
/// when nothing else does, which is the property an offline resolver needs.
public struct ImportConnectivity: Sendable {
    private let sync: any SyncPort

    public init(sync: any SyncPort) {
        self.sync = sync
    }

    /// Whether the device is definitely without a usable connection.
    ///
    /// `false` when the class cannot be read at all — guessing "offline" from an
    /// unreadable state would blame the network for a bug.
    public func isOffline() async -> Bool {
        guard let connection = try? await sync.status().connectionClass else { return false }
        return !connection.isUsable
    }

    /// Classify a thrown port error into the phase a screen should render.
    ///
    /// Offline wins over the code: a request that failed while the radio was off
    /// failed *because* it was off, whatever code the transport invented on the
    /// way out.
    public func phase(for error: any Error) async -> ImportPhase {
        if await isOffline() { return .offline }
        guard let capsuleError = error as? CapsuleError else {
            return .failed(.unknown(ImportPhase.unclassifiedErrorKey))
        }
        return .failed(capsuleError.code)
    }
}

public extension ImportPhase {
    /// The catalog key used when a screen catches something that is not a
    /// ``CapsuleError``. Carried verbatim rather than coerced onto a real code,
    /// so a lookup miss is visible instead of a plausible wrong message.
    static let unclassifiedErrorKey = "error.client.unclassified"
}
