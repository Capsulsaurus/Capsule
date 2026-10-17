import Foundation
import Testing

import CapsuleDomain

/// The error-code catalog and the client recovery matrix
/// (*Upload Protocol — Error Taxonomy*, *Validation*).
@Suite("Error codes map to the documented client recovery")
struct ErrorCodeTests {
    // MARK: The normative five

    @Test("offset_mismatch re-aligns via HEAD")
    func offsetMismatch() {
        #expect(ErrorCode.uploadOffsetMismatch.recoveryAction == .realignViaHead)
    }

    @Test("session_not_found re-creates the session")
    func sessionNotFound() {
        #expect(ErrorCode.uploadSessionNotFound.recoveryAction == .recreateSession)
    }

    @Test("duplicate_blob resolves as a merge, never a re-transfer")
    func duplicateBlob() {
        #expect(ErrorCode.uploadDuplicateBlob.recoveryAction == .mergeExistingBlob)
    }

    @Test("an unsupported protocol version aborts with an upgrade prompt")
    func protocolUnsupported() {
        // There is no negotiation: a client either speaks a version the server
        // accepts, or it does not upload. Retrying would spin forever.
        #expect(ErrorCode.protocolVersionUnsupported.recoveryAction == .abortWithUpgrade)
        #expect(!ErrorCode.protocolVersionUnsupported.isTransient)
    }

    @Test("checksum_mismatch re-sends the same chunk")
    func checksumMismatch() {
        // Nothing was persisted and the offset is unchanged, so the same chunk
        // goes again — not a session re-create, which would discard good bytes.
        #expect(ErrorCode.uploadChecksumMismatch.recoveryAction == .resendChunk)
    }

    @Test("the five normative recoveries are all distinct")
    func normativeRecoveriesAreDistinct() {
        let actions: [RecoveryAction] = [
            ErrorCode.uploadOffsetMismatch.recoveryAction,
            ErrorCode.uploadSessionNotFound.recoveryAction,
            ErrorCode.uploadDuplicateBlob.recoveryAction,
            ErrorCode.protocolVersionUnsupported.recoveryAction,
            ErrorCode.uploadChecksumMismatch.recoveryAction,
        ]
        #expect(Set(actions).count == 5)
    }

    // MARK: Catalog integrity

    @Test("every code carries the exact catalog key, namespaced under error")
    func codesAreCatalogKeys() {
        for code in ErrorCode.knownCases {
            #expect(code.rawValue.hasPrefix("error."), "\(code.rawValue) is not in the error namespace")
            #expect(code.rawValue.split(separator: ".").count >= 3, "\(code.rawValue) has no name segment")
            #expect(code.rawValue.lowercased() == code.rawValue, "\(code.rawValue) is not lowercase")
        }
    }

    @Test("no two codes share a raw value")
    func codesAreUnique() {
        #expect(Set(ErrorCode.knownCases.map(\.rawValue)).count == ErrorCode.knownCases.count)
    }

    @Test("a code from a newer server round-trips and surfaces rather than guessing")
    func unknownCodeSurfaces() {
        let future = ErrorCode(rawValue: "error.upload.some_future_condition")
        #expect(!future.isKnown)
        #expect(future.rawValue == "error.upload.some_future_condition")
        #expect(future.namespace == "upload")
        // Guessing a recovery for a condition this build does not understand
        // could make things worse, so it is surfaced truthfully instead.
        #expect(future.recoveryAction == .surfaceToUser)
    }

    @Test("the namespace is extracted from the code, not stored separately")
    func namespaceExtraction() {
        #expect(ErrorCode.quotaExceeded.namespace == "quota")
        #expect(ErrorCode.federationCircuitOpen.namespace == "federation")
        #expect(ErrorCode.blobPendingUpload.namespace == "blob")
    }

    // MARK: Transience

    @Test("pending_upload is transient, not a durability loss")
    func pendingUploadIsTransient() {
        // Explicitly distinct from 410 Gone: the asset exists and its original
        // is still on another device under a staged upload policy. Showing this
        // as a failure would train users to distrust a working system.
        #expect(ErrorCode.blobPendingUpload.isTransient)
        #expect(ErrorCode.blobPendingUpload.recoveryAction == .retryWithBackoff)
    }

    @Test("rate limits across every namespace retry with backoff")
    func rateLimitsRetry() {
        for code in ErrorCode.knownCases where code.rawValue.hasSuffix("rate_limited") {
            #expect(code.isTransient, "\(code.rawValue) should retry with backoff")
        }
    }

    @Test("a quota rejection is surfaced, never retried into")
    func quotaSurfaces() {
        // Retrying an over-quota upload burns battery and changes nothing; only
        // the user can free space.
        #expect(ErrorCode.quotaExceeded.recoveryAction == .surfaceToUser)
        #expect(!ErrorCode.quotaExceeded.isTransient)
    }

    @Test("a stale-revival or capability-expiry refreshes local state first")
    func staleStateRefreshes() {
        #expect(ErrorCode.uploadStaleRevival.recoveryAction == .refreshAndRetry)
        #expect(ErrorCode.federationCapabilityExpired.recoveryAction == .refreshAndRetry)
        #expect(ErrorCode.directoryVersionConflict.recoveryAction == .refreshAndRetry)
    }

    @Test("a malformed request this client sent is a defect, not a retry")
    func clientDefectsAreReported() {
        #expect(ErrorCode.uploadMalformedRequest.recoveryAction == .reportAsDefect)
        #expect(ErrorCode.uploadChunkNotAligned.recoveryAction == .reportAsDefect)
        #expect(ErrorCode.uploadStorageInconsistent.recoveryAction == .reportAsDefect)
    }

    // MARK: CapsuleError

    @Test("CapsuleError takes its recovery and localization key from its code")
    func errorDelegatesToCode() {
        let error = CapsuleError(
            code: .uploadOffsetMismatch,
            detail: "expected offset 4096, got 8192",
            httpStatus: 409
        )
        #expect(error.recoveryAction == .realignViaHead)
        #expect(error.localizationKey == "error.upload.offset_mismatch")
        // The detail is an English diagnostic for logs — never display copy.
        #expect(error.detail == "expected offset 4096, got 8192")
    }
}
