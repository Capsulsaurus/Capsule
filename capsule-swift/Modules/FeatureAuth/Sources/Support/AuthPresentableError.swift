import CapsuleDomain
import Foundation

// MARK: - AuthErrorKind

/// How a failure should be *presented*, derived from the documented recovery
/// matrix rather than from a network taxonomy this layer would have to invent.
///
/// ``CapsuleError/recoveryAction`` already answers "what should the client do
/// about this", and *Error Handling* makes that mapping normative. Presentation
/// follows it, so a screen never acquires its own private opinion about what a
/// code means.
public enum AuthErrorKind: Sendable, Equatable, Hashable {
    /// The server could not be reached, or the condition clears on its own.
    /// Rendered as the offline/unreachable state with a Retry affordance.
    case temporarilyUnavailable
    /// A person has to do something — fix a credential, free some space, pick a
    /// different handle.
    case actionable
    /// This build and the server disagree about the protocol version. There is
    /// no negotiation; the user must update.
    case upgradeRequired
    /// Not the user's to fix. Offer a support report rather than a retry.
    case defect
}

// MARK: - AuthPresentableError

/// One failure, in the shape a view needs it.
///
/// The **only** display string it carries is a catalog key: ``messageKey`` is
/// the error code's raw value, which *i18n — Server Error Codes* fixes as the
/// catalog key for that condition. ``diagnosticDetail`` is the English
/// engineering message and is for logs and support bundles only — rendering it
/// to a user is a localisation bug, so it is named to say so.
public struct AuthPresentableError: Sendable, Equatable, Hashable, Identifiable {
    public var code: ErrorCode
    public var kind: AuthErrorKind
    /// English diagnostic. Never display copy.
    public var diagnosticDetail: String?

    public var id: String { code.rawValue }

    /// The catalog key for the user-facing message: `error.auth.rate_limited`,
    /// `error.auth.invalid_credentials`, `error.auth.user_already_exists`, and
    /// every other `error.*` key, verbatim from the code.
    public var messageKey: String { code.rawValue }

    /// Whether offering "Try again" is honest for this condition.
    public var isRetryable: Bool {
        kind == .temporarilyUnavailable
    }

    /// Whether this reads as "we cannot reach the server right now", which is
    /// the state every screen in this module has to render.
    public var isOffline: Bool {
        kind == .temporarilyUnavailable
    }

    public init(code: ErrorCode, kind: AuthErrorKind, diagnosticDetail: String? = nil) {
        self.code = code
        self.kind = kind
        self.diagnosticDetail = diagnosticDetail
    }

    /// Classify a thrown error for display.
    ///
    /// A non-``CapsuleError`` is reported as a defect rather than guessed at:
    /// every port throws ``CapsuleError``, so anything else is this client
    /// misbehaving, and saying "check your connection" about a programming
    /// error sends the user to fix the wrong thing.
    public init(_ error: any Error) {
        guard let capsule = error as? CapsuleError else {
            self.init(code: .unknown("error.client.unexpected"), kind: .defect)
            return
        }
        self.init(capsule)
    }

    public init(_ error: CapsuleError) {
        let kind: AuthErrorKind = switch error.recoveryAction {
        case .retryWithBackoff, .refreshAndRetry, .realignViaHead, .recreateSession, .resendChunk:
            .temporarilyUnavailable
        case .abortWithUpgrade:
            .upgradeRequired
        case .surfaceToUser, .mergeExistingBlob:
            .actionable
        case .reportAsDefect:
            .defect
        }
        self.init(code: error.code, kind: kind, diagnosticDetail: error.detail)
    }
}
