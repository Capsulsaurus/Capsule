import Foundation

// MARK: - RecoveryAction

/// What a client should *do* about an error.
///
/// The five upload recoveries are normative — *Upload Protocol — Validation*
/// specifies the client's recovery matrix directly, and the SDK's conformance
/// test replays every taxonomy code against it. Encoding the matrix here rather
/// than in each call site is what keeps the upload path from acquiring three
/// slightly different opinions about what a `409` means.
public enum RecoveryAction: Sendable, Equatable, Hashable, CaseIterable {
    /// Query the authoritative offset and continue from there. No bytes the
    /// server already holds are re-sent.
    case realignViaHead
    /// The session is gone — discarded, expired, or never existed, all
    /// deliberately indistinguishable. Create a new session and re-upload that
    /// blob from zero.
    case recreateSession
    /// The content already exists server-side. Resolve as a **merge**: link the
    /// stored blob to the new asset reference rather than transferring it
    /// again. Merge is strictly additive — it never deletes a blob or rewrites
    /// a manifest.
    case mergeExistingBlob
    /// Stop and tell the user to update. There is no negotiation: a client
    /// either speaks a protocol version the server accepts, or it does not
    /// upload.
    case abortWithUpgrade
    /// Nothing was persisted and the offset is unchanged — re-send the same
    /// chunk.
    case resendChunk
    /// Transient. Retry on the surface's ``RetryClass`` ladder.
    case retryWithBackoff
    /// Local state is stale. Refresh it — membership, capability, the device
    /// directory, the revocation list — then retry once.
    case refreshAndRetry
    /// Nothing the client can do automatically; the user must act (free space,
    /// re-authenticate, contact an admin).
    case surfaceToUser
    /// Not recoverable, and not the user's to fix. Report it.
    case reportAsDefect
}

public extension ErrorCode {
    /// The documented client recovery for this code.
    ///
    /// The upload rows are the normative recovery matrix; the rest follow the
    /// same reasoning — anything transient retries, anything caused by stale
    /// local state refreshes, anything requiring a human decision surfaces, and
    /// a server-side inconsistency is reported rather than retried into.
    var recoveryAction: RecoveryAction {
        switch self {
        // The five normative upload recoveries.
        case .uploadOffsetMismatch: .realignViaHead

        case .uploadSessionNotFound: .recreateSession

        case .uploadDuplicateBlob: .mergeExistingBlob

        case .protocolVersionUnsupported: .abortWithUpgrade

        case .uploadChecksumMismatch: .resendChunk

        // Transient by construction.
        case .uploadFinalizeInProgress,
             .uploadSessionNotActive,
             .uploadReceiptNotAvailable,
             .blobPendingUpload,
             .authRateLimited,
             .enrollmentRateLimited,
             .shareRateLimited,
             .dropRateLimited,
             .storageDeepRateLimited,
             .moderationReportRateLimited,
             .federationRateBudgetExceeded,
             .federationCircuitOpen:
            .retryWithBackoff

        // Local state is behind; refresh it, then retry once.
        case .directoryVersionConflict,
             .syncCursorInvalid,
             .uploadStaleRevival,
             .uploadAmkRegressed,
             .federationCapabilityExpired,
             .uploadAlbumAccessDenied:
            .refreshAndRetry

        // A person has to do something.
        case .quotaExceeded,
             .quotaGraceLocked,
             .quotaPeerBudgetExceeded,
             .authInvalidCredentials,
             .authUserAlreadyExists,
             .authRevokeProofRequired,
             .authRevokeProofInvalid,
             .enrollmentLocalAuthRequired,
             .enrollmentCodeRefused,
             .enrollmentChannelNotFound,
             .syncUnauthenticated,
             .moderationAccountSuspended,
             .moderationServerBlocked,
             .dropPassphraseRequired,
             .dropCapExceeded,
             .dropNotInInbox,
             .uploadFileTooLarge,
             .uploadOwnerNotPermitted,
             .uploadForbidden,
             .uploadDeviceNotAuthorized,
             .albumNotAvailable,
             .federationCapabilityRevoked,
             .federationCapabilityInvalid,
             .federationAudienceMismatch,
             .federationScopeInsufficient:
            .surfaceToUser

        // A defect: the client sent something it should never have sent, or the
        // server contradicted itself. Retrying reproduces it.
        case .uploadMalformedRequest,
             .uploadUnknownCryptoSuite,
             .uploadInvalidHash,
             .uploadInvalidSize,
             .uploadUnsupportedContentType,
             .uploadTimestampOutOfRange,
             .uploadEnvelopeMismatch,
             .uploadEnvelopeRejected,
             .uploadUnsupportedMediaType,
             .uploadEmptyChunk,
             .uploadChunkNotAligned,
             .uploadChunkTooLarge,
             .uploadMissingOffset,
             .uploadMissingChecksum,
             .uploadChunkConflict,
             .uploadSizeExceeded,
             .uploadContentHashMismatch,
             .uploadStorageInconsistent,
             .uploadInvalidAction,
             .albumInvalidID,
             .directoryMalformed,
             .escrowMalformed,
             .enrollmentRelayMalformed,
             .storageInvalidRequest,
             .dropMalformedDescriptor,
             .moderationReportUnsigned:
            .reportAsDefect

        // A code from a newer server. Surface it truthfully rather than
        // guessing at a recovery that might make things worse.
        case .unknown:
            .surfaceToUser
        }
    }

    /// Whether the condition resolves on its own if the client simply waits.
    var isTransient: Bool {
        recoveryAction == .retryWithBackoff
    }
}

// MARK: - CapsuleError

/// The one error type every port throws.
///
/// A single type, carrying a **stable code**, so a view model never has to
/// pattern-match across a dozen error enums to decide what to show. The rules
/// it enforces by construction:
///
/// - ``code`` is always present and always drawn from the catalog namespace, so
///   a user-facing string can always be resolved.
/// - ``detail`` is an **English diagnostic**, never display copy. It goes in
///   logs and support reports. Rendering it to a user is a localisation bug.
/// - ``recoveryAction`` comes from the code, so every call site recovers the
///   same way.
public struct CapsuleError: Error, Sendable, Equatable, Hashable {
    /// The stable code.
    public var code: ErrorCode
    /// The English detail message, for logs and support reports only.
    public var detail: String?
    /// The HTTP status, when the error came from a server response.
    public var httpStatus: Int?
    /// The underlying error's description, when this wraps a transport or
    /// platform failure.
    public var underlyingDescription: String?

    public init(
        code: ErrorCode,
        detail: String? = nil,
        httpStatus: Int? = nil,
        underlyingDescription: String? = nil
    ) {
        self.code = code
        self.detail = detail
        self.httpStatus = httpStatus
        self.underlyingDescription = underlyingDescription
    }

    /// What the caller should do about it.
    public var recoveryAction: RecoveryAction {
        code.recoveryAction
    }

    /// Whether waiting and retrying is the right response.
    public var isTransient: Bool {
        code.isTransient
    }

    /// The catalog key a client looks the user-facing message up by. Identical
    /// to the code's raw value; named so call sites read as what they are.
    public var localizationKey: String {
        code.rawValue
    }
}
