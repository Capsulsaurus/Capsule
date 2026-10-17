import CapsuleDomain
import CapsuleMock
import Foundation

// MARK: - PreviewRestore

/// A ``RestorePort`` over ``MockEnvironment``.
///
/// The diff it reports is deliberately not all-green: it carries conflicts and
/// a superseded-by-local bucket, because those are the outcomes the dry-run
/// screen exists to show. A mock that always reported "everything would be
/// added" would leave the conservative-reconciliation copy — newer local state
/// wins, nothing is silently overwritten — untested and unseen.
public actor PreviewRestore: RestorePort {
    private let clock: MockClock
    private let seed: UInt64
    private let ledgerIsComplete: Bool
    private let signatureChainIsIntact: Bool
    private var hasRunDryRun = false

    public init(
        environment: MockEnvironment,
        ledgerIsComplete: Bool = true,
        signatureChainIsIntact: Bool = true
    ) {
        clock = environment.configuration.clock
        seed = environment.configuration.seed
        self.ledgerIsComplete = ledgerIsComplete
        self.signatureChainIsIntact = signatureChainIsIntact
    }

    public func preview(artifact _: URL) async throws -> RestorePreview {
        RestorePreview(
            assetCount: 12480,
            totalBytes: 214748364800,
            exportedAt: clock.offset(days: -181),
            exporterModel: "Mac16,7",
            artifactVersion: 1
        )
    }

    public func dryRun(artifact _: URL) async throws -> RestoreDiff {
        hasRunDryRun = true
        return RestoreDiff(
            addedCount: 11902,
            alreadyPresentCount: 502,
            conflictingCount: 61,
            supersededByLocalCount: 15,
            amkLedgerIsComplete: ledgerIsComplete,
            signatureChainIsIntact: signatureChainIsIntact
        )
    }

    /// Refuses without a dry run, and refuses a wrong phrase.
    ///
    /// The port checking the phrase as well as the screen is the point: a gate
    /// enforced only in a view is a gate that a scripted caller, a deep link, or
    /// a future refactor walks straight past.
    public func commit(artifact: URL, confirmationPhrase: String) async throws -> RestoreDiff {
        guard hasRunDryRun else {
            throw CapsuleError(code: .escrowMalformed, detail: "CapsuleMock: dry run has not been run")
        }
        guard confirmationPhrase == "RESTORE" else {
            throw CapsuleError(code: .escrowMalformed, detail: "CapsuleMock: confirmation phrase mismatch")
        }
        let diff = try await dryRun(artifact: artifact)
        guard diff.isCommittable else {
            throw CapsuleError(code: .escrowMalformed, detail: "CapsuleMock: artifact failed verification")
        }
        return diff
    }

    public func shamirShares() async throws -> [ShamirShareSummary] {
        [
            ShamirShareSummary(id: "share-1", label: "Safe deposit box", issuedAt: clock.offset(days: -400)),
            ShamirShareSummary(id: "share-2", label: "Password manager", issuedAt: clock.offset(days: -400)),
            ShamirShareSummary(
                id: "share-3",
                label: "Sister's house",
                issuedAt: clock.offset(days: -400),
                isInvalidated: true
            ),
        ]
    }

    /// Reconstructs only from a quorum, and only from live shares — an
    /// invalidated share is dead material and must not silently count towards
    /// the threshold.
    public func reconstructSecret(fromShareIDs ids: [String]) async throws -> RedactedSecret {
        let live = try await shamirShares().filter { !$0.isInvalidated }.map(\.id)
        let usable = ids.filter { live.contains($0) }
        guard usable.count >= 2 else {
            throw CapsuleError(code: .escrowMalformed, detail: "CapsuleMock: quorum not met")
        }
        return RedactedSecret(MockHash.hex(MockHash.mix(seed), digits: 32))
    }
}
