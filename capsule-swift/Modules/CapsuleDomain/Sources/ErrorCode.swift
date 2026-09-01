import Foundation

// MARK: - ErrorCode

/// The stable `error.*` codes the server sends, mirrored from the canonical
/// i18n catalog at `locales/en.json` (*i18n — Server Error Codes*).
///
/// The contract, and the reason this is an enum rather than a string:
///
/// - **Clients switch on codes, never on bare HTTP statuses.** Two conditions
///   can share a status and need entirely different recoveries — a `409` is a
///   duplicate blob (merge) or a stale offset (re-align) or a session already
///   finalizing (re-query), and only the code distinguishes them.
/// - **The code is stable; the message is not.** The English detail message
///   stays English and is a diagnostic; the *user-facing* string is looked up
///   from the catalog by this code. No display copy lives in this module.
/// - **An unrecognised code is carried verbatim**, never coerced. A newer
///   server may send a code this build predates, and the app must still report
///   something truthful and log the exact value.
///
/// Every case here corresponds one-to-one with an `error.*` key in the
/// catalog; the raw strings are the keys themselves, so a mismatch is a
/// lookup miss rather than a silent wrong message.
public enum ErrorCode: ClosedWireEnum {
    // MARK: Protocol handshake

    case protocolVersionUnsupported

    // MARK: Authentication

    case authUserAlreadyExists
    case authInvalidCredentials
    case authRateLimited
    case authRevokeProofRequired
    case authRevokeProofInvalid

    // MARK: Device enrollment

    case enrollmentLocalAuthRequired
    case enrollmentRateLimited
    case enrollmentCodeRefused
    case enrollmentChannelNotFound
    case enrollmentRelayMalformed

    // MARK: Master-key escrow

    case escrowMalformed

    // MARK: Device directory

    case directoryVersionConflict
    case directoryMalformed

    // MARK: Album provisioning

    case albumInvalidID
    case albumNotAvailable

    // MARK: Upload protocol

    case uploadMalformedRequest
    case uploadUnknownCryptoSuite
    case uploadInvalidHash
    case uploadInvalidSize
    case uploadFileTooLarge
    case uploadUnsupportedContentType
    case uploadAlbumAccessDenied
    case uploadDeviceNotAuthorized
    case uploadTimestampOutOfRange
    case uploadEnvelopeMismatch
    case uploadOwnerNotPermitted
    case uploadDuplicateBlob
    case uploadUnsupportedMediaType
    case uploadEmptyChunk
    case uploadChunkNotAligned
    case uploadChunkTooLarge
    case uploadMissingOffset
    case uploadMissingChecksum
    case uploadChecksumMismatch
    case uploadOffsetMismatch
    case uploadChunkConflict
    case uploadSizeExceeded
    case uploadSessionNotFound
    case uploadSessionNotActive
    case uploadFinalizeInProgress
    case uploadReceiptNotAvailable
    case uploadContentHashMismatch
    case uploadEnvelopeRejected
    case uploadForbidden
    case uploadStorageInconsistent
    case uploadInvalidAction
    case uploadStaleRevival
    case uploadAmkRegressed

    // MARK: Blob fetch

    case blobPendingUpload

    // MARK: Sync feed

    case syncCursorInvalid
    case syncUnauthenticated

    // MARK: Storage verification

    case storageInvalidRequest
    case storageDeepRateLimited

    // MARK: Quota

    case quotaExceeded
    case quotaGraceLocked
    case quotaPeerBudgetExceeded

    // MARK: Share links

    case shareRateLimited

    // MARK: Web-upload drops

    case dropCapExceeded
    case dropMalformedDescriptor
    case dropRateLimited
    case dropPassphraseRequired
    case dropNotInInbox

    // MARK: Federation

    case federationCapabilityInvalid
    case federationCapabilityExpired
    case federationCapabilityRevoked
    case federationAudienceMismatch
    case federationScopeInsufficient
    case federationRateBudgetExceeded
    case federationCircuitOpen

    // MARK: Moderation

    case moderationAccountSuspended
    case moderationServerBlocked
    case moderationReportUnsigned
    case moderationReportRateLimited

    /// A code from a newer server than this build. Preserved verbatim for
    /// logging and for a catalog lookup that may still succeed.
    case unknown(String)

    public static let knownCases: [ErrorCode] = [
        .protocolVersionUnsupported,
        .authUserAlreadyExists,
        .authInvalidCredentials,
        .authRateLimited,
        .authRevokeProofRequired,
        .authRevokeProofInvalid,
        .enrollmentLocalAuthRequired,
        .enrollmentRateLimited,
        .enrollmentCodeRefused,
        .enrollmentChannelNotFound,
        .enrollmentRelayMalformed,
        .escrowMalformed,
        .directoryVersionConflict,
        .directoryMalformed,
        .albumInvalidID,
        .albumNotAvailable,
        .uploadMalformedRequest,
        .uploadUnknownCryptoSuite,
        .uploadInvalidHash,
        .uploadInvalidSize,
        .uploadFileTooLarge,
        .uploadUnsupportedContentType,
        .uploadAlbumAccessDenied,
        .uploadDeviceNotAuthorized,
        .uploadTimestampOutOfRange,
        .uploadEnvelopeMismatch,
        .uploadOwnerNotPermitted,
        .uploadDuplicateBlob,
        .uploadUnsupportedMediaType,
        .uploadEmptyChunk,
        .uploadChunkNotAligned,
        .uploadChunkTooLarge,
        .uploadMissingOffset,
        .uploadMissingChecksum,
        .uploadChecksumMismatch,
        .uploadOffsetMismatch,
        .uploadChunkConflict,
        .uploadSizeExceeded,
        .uploadSessionNotFound,
        .uploadSessionNotActive,
        .uploadFinalizeInProgress,
        .uploadReceiptNotAvailable,
        .uploadContentHashMismatch,
        .uploadEnvelopeRejected,
        .uploadForbidden,
        .uploadStorageInconsistent,
        .uploadInvalidAction,
        .uploadStaleRevival,
        .uploadAmkRegressed,
        .blobPendingUpload,
        .syncCursorInvalid,
        .syncUnauthenticated,
        .storageInvalidRequest,
        .storageDeepRateLimited,
        .quotaExceeded,
        .quotaGraceLocked,
        .quotaPeerBudgetExceeded,
        .shareRateLimited,
        .dropCapExceeded,
        .dropMalformedDescriptor,
        .dropRateLimited,
        .dropPassphraseRequired,
        .dropNotInInbox,
        .federationCapabilityInvalid,
        .federationCapabilityExpired,
        .federationCapabilityRevoked,
        .federationAudienceMismatch,
        .federationScopeInsufficient,
        .federationRateBudgetExceeded,
        .federationCircuitOpen,
        .moderationAccountSuspended,
        .moderationServerBlocked,
        .moderationReportUnsigned,
        .moderationReportRateLimited,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .protocolVersionUnsupported: "error.protocol.version_unsupported"
        case .authUserAlreadyExists: "error.auth.user_already_exists"
        case .authInvalidCredentials: "error.auth.invalid_credentials"
        case .authRateLimited: "error.auth.rate_limited"
        case .authRevokeProofRequired: "error.auth.revoke_proof_required"
        case .authRevokeProofInvalid: "error.auth.revoke_proof_invalid"
        case .enrollmentLocalAuthRequired: "error.enrollment.local_auth_required"
        case .enrollmentRateLimited: "error.enrollment.rate_limited"
        case .enrollmentCodeRefused: "error.enrollment.code_refused"
        case .enrollmentChannelNotFound: "error.enrollment.channel_not_found"
        case .enrollmentRelayMalformed: "error.enrollment.relay_malformed"
        case .escrowMalformed: "error.escrow.malformed"
        case .directoryVersionConflict: "error.directory.version_conflict"
        case .directoryMalformed: "error.directory.malformed"
        case .albumInvalidID: "error.album.invalid_id"
        case .albumNotAvailable: "error.album.not_available"
        case .uploadMalformedRequest: "error.upload.malformed_request"
        case .uploadUnknownCryptoSuite: "error.upload.unknown_crypto_suite"
        case .uploadInvalidHash: "error.upload.invalid_hash"
        case .uploadInvalidSize: "error.upload.invalid_size"
        case .uploadFileTooLarge: "error.upload.file_too_large"
        case .uploadUnsupportedContentType: "error.upload.unsupported_content_type"
        case .uploadAlbumAccessDenied: "error.upload.album_access_denied"
        case .uploadDeviceNotAuthorized: "error.upload.device_not_authorized"
        case .uploadTimestampOutOfRange: "error.upload.timestamp_out_of_range"
        case .uploadEnvelopeMismatch: "error.upload.envelope_mismatch"
        case .uploadOwnerNotPermitted: "error.upload.owner_not_permitted"
        case .uploadDuplicateBlob: "error.upload.duplicate_blob"
        case .uploadUnsupportedMediaType: "error.upload.unsupported_media_type"
        case .uploadEmptyChunk: "error.upload.empty_chunk"
        case .uploadChunkNotAligned: "error.upload.chunk_not_aligned"
        case .uploadChunkTooLarge: "error.upload.chunk_too_large"
        case .uploadMissingOffset: "error.upload.missing_offset"
        case .uploadMissingChecksum: "error.upload.missing_checksum"
        case .uploadChecksumMismatch: "error.upload.checksum_mismatch"
        case .uploadOffsetMismatch: "error.upload.offset_mismatch"
        case .uploadChunkConflict: "error.upload.chunk_conflict"
        case .uploadSizeExceeded: "error.upload.size_exceeded"
        case .uploadSessionNotFound: "error.upload.session_not_found"
        case .uploadSessionNotActive: "error.upload.session_not_active"
        case .uploadFinalizeInProgress: "error.upload.finalize_in_progress"
        case .uploadReceiptNotAvailable: "error.upload.receipt_not_available"
        case .uploadContentHashMismatch: "error.upload.content_hash_mismatch"
        case .uploadEnvelopeRejected: "error.upload.envelope_rejected"
        case .uploadForbidden: "error.upload.forbidden"
        case .uploadStorageInconsistent: "error.upload.storage_inconsistent"
        case .uploadInvalidAction: "error.upload.invalid_action"
        case .uploadStaleRevival: "error.upload.stale_revival"
        case .uploadAmkRegressed: "error.upload.amk_regressed"
        case .blobPendingUpload: "error.blob.pending_upload"
        case .syncCursorInvalid: "error.sync.cursor_invalid"
        case .syncUnauthenticated: "error.sync.unauthenticated"
        case .storageInvalidRequest: "error.storage.invalid_request"
        case .storageDeepRateLimited: "error.storage.deep_rate_limited"
        case .quotaExceeded: "error.quota.exceeded"
        case .quotaGraceLocked: "error.quota.grace_locked"
        case .quotaPeerBudgetExceeded: "error.quota.peer_budget_exceeded"
        case .shareRateLimited: "error.share.rate_limited"
        case .dropCapExceeded: "error.drop.cap_exceeded"
        case .dropMalformedDescriptor: "error.drop.malformed_descriptor"
        case .dropRateLimited: "error.drop.rate_limited"
        case .dropPassphraseRequired: "error.drop.passphrase_required"
        case .dropNotInInbox: "error.drop.not_in_inbox"
        case .federationCapabilityInvalid: "error.federation.capability_invalid"
        case .federationCapabilityExpired: "error.federation.capability_expired"
        case .federationCapabilityRevoked: "error.federation.capability_revoked"
        case .federationAudienceMismatch: "error.federation.audience_mismatch"
        case .federationScopeInsufficient: "error.federation.scope_insufficient"
        case .federationRateBudgetExceeded: "error.federation.rate_budget_exceeded"
        case .federationCircuitOpen: "error.federation.circuit_open"
        case .moderationAccountSuspended: "error.moderation.account_suspended"
        case .moderationServerBlocked: "error.moderation.server_blocked"
        case .moderationReportUnsigned: "error.moderation.report_unsigned"
        case .moderationReportRateLimited: "error.moderation.report_rate_limited"
        case let .unknown(raw): raw
        }
    }

    /// The namespace half of the code — `upload`, `quota`, `federation`, … —
    /// for grouping in diagnostics.
    public var namespace: String {
        let parts = rawValue.split(separator: ".")
        return parts.count >= 2 ? String(parts[1]) : ""
    }
}
