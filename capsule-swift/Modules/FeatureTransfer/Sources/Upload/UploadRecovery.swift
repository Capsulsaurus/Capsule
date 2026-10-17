import CapsuleDomain
import SwiftUI

// MARK: - UploadRecoveryOption

/// A failure, and the one thing the protocol says to do about it.
///
/// The five upload rows of the recovery matrix are **normative** (*Upload
/// Protocol — Error Taxonomy*), and the button on screen is labelled with the
/// recovery itself rather than a generic "Try again":
///
/// | Code | HTTP | Button |
/// | --- | --- | --- |
/// | `error.upload.offset_mismatch` | `409` | Re-align with the server |
/// | `error.upload.session_not_found` | `404` | Restart the session |
/// | `error.upload.duplicate_blob` | `409` | Merge with the stored copy |
/// | `error.protocol.version_unsupported` | `426` | Update Capsule |
/// | `error.upload.checksum_mismatch` | `400` | Re-send the chunk |
///
/// A generic label would make two `409`s look like the same problem when their
/// recoveries are opposites — one re-aligns an offset, the other stops
/// transferring entirely and links a blob that is already stored.
public struct UploadRecoveryOption: Sendable, Equatable, Identifiable {
    /// The stable server code. Also the catalog key its message is looked up
    /// by — the code *is* the key (*i18n — Server Error Codes*).
    public var code: ErrorCode
    /// The documented recovery, taken from the domain's matrix rather than
    /// re-decided here.
    public var action: RecoveryAction

    public var id: String { code.rawValue }

    public init(code: ErrorCode) {
        self.code = code
        action = code.recoveryAction
    }

    /// The localized user-facing message for the failure.
    public var messageKey: LocalizedStringKey { LocalizedStringKey(code.rawValue) }

    /// The **button label** — the documented recovery action, in words.
    public var buttonTitleKey: LocalizedStringKey { action.buttonTitleKey }

    /// One line saying what pressing it will do.
    public var explanationKey: LocalizedStringKey { action.explanationKey }

    /// Whether the app can carry the recovery out, or whether it needs a person.
    public var isAutomatable: Bool { action.isAutomatable }

    /// Whether this failure is a hard stop with no in-app remedy.
    ///
    /// `426` is the one that matters: there is **no negotiation and no
    /// downgrade** — a client either speaks a version the server accepts or it
    /// does not upload — so the surface for it is a dedicated screen, not a
    /// button.
    public var requiresProtocolUpgrade: Bool { action == .abortWithUpgrade }
}

// MARK: - UploadFailure

/// One failed session on an asset, with its recovery already resolved.
public struct UploadFailure: Sendable, Equatable, Identifiable {
    public var uploadID: UploadID
    public var tier: UploadTier
    public var option: UploadRecoveryOption

    public var id: UploadID { uploadID }

    public init(uploadID: UploadID, tier: UploadTier, code: ErrorCode) {
        self.uploadID = uploadID
        self.tier = tier
        option = UploadRecoveryOption(code: code)
    }

    /// The failure a terminal ``UploadSessionState/failedProcessing`` session
    /// implies.
    ///
    /// Finalization fails for exactly three reasons — hash mismatch, size
    /// mismatch, or envelope re-validation failure — and all three are reported
    /// as a content-hash mismatch to the client, which is
    /// `error.upload.content_hash_mismatch`: a defect to report, never a retry
    /// loop, because "a mismatch is always treated as corruption or tampering
    /// and is never silently retried" (*Upload Protocol — Finalization and
    /// Integrity*).
    public static func fromTerminal(_ session: UploadSession) -> UploadFailure? {
        guard session.state == .failedProcessing else { return nil }
        return UploadFailure(uploadID: session.id, tier: session.tier, code: .uploadContentHashMismatch)
    }
}
