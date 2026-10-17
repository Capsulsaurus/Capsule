import Foundation
import Testing

import CapsuleDomain
import FeatureTransfer
import SwiftUI

/// The normative recovery matrix (*Upload Protocol — Error Taxonomy*).
///
/// All five rows, because the whole point of switching on codes rather than
/// HTTP statuses is that two `409`s can need opposite recoveries.
@Suite("Upload error to recovery action")
struct UploadRecoveryTests {
    @Test("offset_mismatch re-aligns via HEAD")
    func offsetMismatch() {
        let option = UploadRecoveryOption(code: .uploadOffsetMismatch)

        #expect(option.action == .realignViaHead)
        #expect(option.buttonTitleKey == "app.transfer.recovery.realign")
        #expect(option.isAutomatable)
    }

    @Test("session_not_found restarts the session")
    func sessionNotFound() {
        let option = UploadRecoveryOption(code: .uploadSessionNotFound)

        #expect(option.action == .recreateSession)
        #expect(option.buttonTitleKey == "app.transfer.recovery.restart_session")
    }

    @Test("duplicate_blob merges rather than transferring again")
    func duplicateBlob() {
        let option = UploadRecoveryOption(code: .uploadDuplicateBlob)

        #expect(option.action == .mergeExistingBlob)
        #expect(option.buttonTitleKey == "app.transfer.recovery.merge")
    }

    @Test("a 426 aborts with an upgrade and never negotiates")
    func protocolUnsupported() {
        let option = UploadRecoveryOption(code: .protocolVersionUnsupported)

        #expect(option.action == .abortWithUpgrade)
        #expect(option.buttonTitleKey == "app.transfer.recovery.update_app")
        #expect(option.requiresProtocolUpgrade)
        // Nothing the app can do on its own — offering a retry button would
        // loop forever against a server that will never accept this build.
        #expect(!option.isAutomatable)
    }

    @Test("checksum_mismatch re-sends the same chunk")
    func checksumMismatch() {
        let option = UploadRecoveryOption(code: .uploadChecksumMismatch)

        #expect(option.action == .resendChunk)
        #expect(option.buttonTitleKey == "app.transfer.recovery.resend_chunk")
    }

    @Test("the message is looked up by the stable code, which is the catalog key")
    func messageKeyIsTheCode() {
        let option = UploadRecoveryOption(code: .uploadOffsetMismatch)

        #expect(option.messageKey == LocalizedStringKeyFixture.key(for: option.code.rawValue))
        #expect(option.id == "error.upload.offset_mismatch")
    }

    @Test("only 426 routes to the upgrade screen")
    func onlyUpgradeIsAHardStop() {
        let codes: [ErrorCode] = [
            .uploadOffsetMismatch, .uploadSessionNotFound, .uploadDuplicateBlob, .uploadChecksumMismatch,
        ]

        #expect(codes.allSatisfy { !UploadRecoveryOption(code: $0).requiresProtocolUpgrade })
    }

    @Test("a terminal failedProcessing session reports a content-hash mismatch")
    func terminalFailure() {
        let session = UploadSession(
            id: UploadID("upload-1"),
            assetID: "asset-1",
            blobRole: .original,
            tier: .original,
            state: .failedProcessing,
            offset: 100,
            declaredSize: 100,
            ciphertextHash: BlobHash("hash-1")
        )

        let failure = UploadFailure.fromTerminal(session)

        #expect(failure?.option.code == .uploadContentHashMismatch)
        // Corruption or tampering is never silently retried.
        #expect(failure?.option.action == .reportAsDefect)
    }

    @Test("a session that has not failed produces no failure row")
    func noFailureForLiveSession() {
        let session = UploadSession(
            id: UploadID("upload-2"),
            assetID: "asset-2",
            blobRole: .original,
            tier: .original,
            state: .uploading,
            offset: 10,
            declaredSize: 100,
            ciphertextHash: BlobHash("hash-2")
        )

        #expect(UploadFailure.fromTerminal(session) == nil)
    }
}

// MARK: - Adaptive chunk sizing

/// *Upload Protocol — Adaptive Chunk Sizing*.
@Suite("Adaptive chunk plan")
struct AdaptiveChunkPlanTests {
    @Test("the starting size follows the server's file-size tiers")
    func startingSizes() {
        #expect(AdaptiveChunkPlan.startingSize(declaredSize: 5000000) == 256 * 1024)
        #expect(AdaptiveChunkPlan.startingSize(declaredSize: 50000000) == 1024 * 1024)
        #expect(AdaptiveChunkPlan.startingSize(declaredSize: 500000000) == 4 * 1024 * 1024)
    }

    @Test("no adjustment happens inside the warm-up")
    func warmUpHoldsTheStartingSize() {
        let plan = AdaptiveChunkPlan.make(
            declaredSize: 50000000,
            observedBytesPerSecond: 20000000,
            bytesSentAtCurrentSize: 1024,
            chunksSentAtCurrentSize: 1,
            connection: .unmetered
        )

        #expect(plan.adjustment == .warmingUp)
        #expect(plan.currentBytes == plan.suggestedBytes)
    }

    @Test("sustained throughput above 5 MB/s doubles the chunk size")
    func raisesOnFastLink() {
        let plan = AdaptiveChunkPlan.make(
            declaredSize: 50000000,
            observedBytesPerSecond: 8000000,
            bytesSentAtCurrentSize: 16 * 1024 * 1024,
            chunksSentAtCurrentSize: 12,
            connection: .unmetered
        )

        #expect(plan.adjustment == .raised)
        #expect(plan.currentBytes == 2 * 1024 * 1024)
    }

    @Test("sustained throughput below 1 MB/s halves it, clamped at the floor")
    func lowersOnSlowLink() {
        let plan = AdaptiveChunkPlan.make(
            declaredSize: 5000000,
            observedBytesPerSecond: 200000,
            bytesSentAtCurrentSize: 16 * 1024 * 1024,
            chunksSentAtCurrentSize: 12,
            connection: .metered
        )

        #expect(plan.adjustment == .lowered)
        #expect(plan.currentBytes == AdaptiveChunkPlan.minimumBytes)
    }

    @Test("an adverse link takes the conservative choice regardless of rate")
    func adverseLinkIsConservative() {
        let plan = AdaptiveChunkPlan.make(
            declaredSize: 500000000,
            observedBytesPerSecond: 40000000,
            bytesSentAtCurrentSize: 64 * 1024 * 1024,
            chunksSentAtCurrentSize: 40,
            connection: .adverse
        )

        #expect(plan.adjustment == .conservativeForAdverseLink)
        #expect(plan.currentBytes == AdaptiveChunkPlan.minimumBytes)
    }

    @Test("every candidate size is 4 KiB-aligned by construction")
    func alwaysAligned() {
        let rates: [Double?] = [nil, 200000, 3000000, 40000000]
        let sizes: [UInt64] = [1000, 5000000, 50000000, 5000000000]

        for size in sizes {
            for rate in rates {
                let plan = AdaptiveChunkPlan.make(
                    declaredSize: size,
                    observedBytesPerSecond: rate,
                    bytesSentAtCurrentSize: 32 * 1024 * 1024,
                    chunksSentAtCurrentSize: 20,
                    connection: .unmetered
                )
                #expect(plan.isAligned)
                #expect(plan.currentBytes >= AdaptiveChunkPlan.minimumBytes)
                #expect(plan.currentBytes <= AdaptiveChunkPlan.maximumBytes)
            }
        }
    }
}

// MARK: - Fixture

/// Builds a `LocalizedStringKey` from a runtime string, so a test can assert
/// that a key really is the error code rather than a hand-written duplicate.
enum LocalizedStringKeyFixture {
    static func key(for raw: String) -> LocalizedStringKey {
        LocalizedStringKey(raw)
    }
}
