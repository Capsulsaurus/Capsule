import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

/// One row of the committability table: the two verification verdicts, and
/// whether a commit may be offered at all.
struct CommittabilitySample: Sendable {
    let ledger: Bool
    let signature: Bool
    let committable: Bool
}

// MARK: - RestoreModeTests

/// The three modes are ordered so a screen can prove it never offers a later
/// one before an earlier one has run.
@Suite("Restore modes escalate: preview, dry run, commit")
struct RestoreModeTests {
    @Test("the modes are ordered by consequence")
    func modesAreOrderedByConsequence() {
        #expect(RestoreMode.allCases == [.preview, .dryRun, .commit])
        #expect(RestoreMode.preview < RestoreMode.dryRun)
        #expect(RestoreMode.dryRun < RestoreMode.commit)
        #expect(RestoreMode.dryRun.rawValue == "dry_run")
    }

    @Test("both verification checks are refusals, not warnings", arguments: [
        CommittabilitySample(ledger: true, signature: true, committable: true),
        CommittabilitySample(ledger: false, signature: true, committable: false),
        CommittabilitySample(ledger: true, signature: false, committable: false),
        CommittabilitySample(ledger: false, signature: false, committable: false),
    ])
    func committabilityNeedsBothChecks(sample: CommittabilitySample) {
        let diff = RestoreDiff(
            addedCount: 1,
            alreadyPresentCount: 0,
            conflictingCount: 0,
            supersededByLocalCount: 0,
            amkLedgerIsComplete: sample.ledger,
            signatureChainIsIntact: sample.signature
        )

        #expect(diff.isCommittable == sample.committable)
    }
}

// MARK: - RestoreFlowTests

/// Dry run is the default and commit is never the default.
@Suite("A restore commits only after a dry run and a typed phrase")
@MainActor
struct RestoreFlowTests {
    private static let artifact = URL(fileURLWithPath: "/Backups/capsule-2026-02-22.tar")

    private static func model(
        restore: StubRestorePort = StubRestorePort(),
        recovery: StubRecoveryPort = StubRecoveryPort(restoreSecret: StubRestorePort.reconstructedSecret)
    ) -> RestoreFlowViewModel {
        RestoreFlowViewModel(artifact: artifact, restore: restore, recovery: recovery)
    }

    @Test("a fresh flow starts in preview with nothing committable")
    func flowStartsInPreview() {
        let model = Self.model()

        #expect(model.mode == .preview)
        #expect(model.preview == nil)
        #expect(model.diff == nil)
        #expect(!model.hasCommittableDiff)
        #expect(!model.canCommit)
        #expect(model.requiredPhrase == "RESTORE")
    }

    @Test("preview reports the artifact's shape without decrypting or writing")
    func previewReportsShape() async {
        let model = Self.model()

        await model.runPreview()

        #expect(model.mode == .preview)
        #expect(model.preview?.assetCount == 12480)
        #expect(model.state == .ready)
        #expect(model.diff == nil, "preview must not produce a diff")
        #expect(!model.canCommit)
    }

    @Test("the dry run produces the diff a commit would apply, and still writes nothing")
    func dryRunProducesTheDiff() async {
        let model = Self.model()
        await model.runPreview()

        await model.runDryRun()

        #expect(model.mode == .dryRun)
        #expect(model.diff?.conflictingCount == 61)
        #expect(model.hasCommittableDiff)
        #expect(!model.isRefused)
        #expect(model.committedDiff == nil, "a dry run commits nothing")
        #expect(!model.canCommit, "the phrase has not been typed")
    }

    @Test(
        "the confirmation phrase is compared exactly, trimmed only of surrounding whitespace",
        arguments: [
            (typed: "RESTORE", matches: true),
            (typed: "  RESTORE  ", matches: true),
            (typed: "RESTORE\n", matches: true),
            (typed: "restore", matches: false),
            (typed: "Restore", matches: false),
            (typed: "RE STORE", matches: false),
            (typed: "RESTORE!", matches: false),
            (typed: "", matches: false),
        ]
    )
    func phraseComparisonIsDeliberate(sample: (typed: String, matches: Bool)) {
        let model = Self.model()

        model.confirmationInput = sample.typed

        #expect(model.confirmationMatches == sample.matches)
    }

    @Test("committing before the dry run is refused locally and never reaches the port")
    func commitBeforeDryRunIsRefused() async {
        let port = StubRestorePort()
        let model = Self.model(restore: port)
        await model.runPreview()
        model.confirmationInput = "RESTORE"

        let committed = await model.commit()

        #expect(!committed)
        let attempts = await port.commitAttempts
        #expect(attempts.isEmpty, "a refused commit must not be attempted at all")
        #expect(model.committedDiff == nil)
    }

    @Test("a wrong phrase is refused locally and never reaches the port")
    func wrongPhraseIsRefused() async {
        let port = StubRestorePort()
        let model = Self.model(restore: port)
        await model.runDryRun()
        model.confirmationInput = "restore"

        let committed = await model.commit()

        #expect(!committed)
        let attempts = await port.commitAttempts
        #expect(attempts.isEmpty)
    }

    @Test("a dry run plus the exact phrase is what commits, and the field is cleared after")
    func gateSatisfiedCommits() async {
        let port = StubRestorePort()
        let model = Self.model(restore: port)
        await model.runDryRun()
        model.confirmationInput = " RESTORE "
        #expect(model.canCommit)

        let committed = await model.commit()

        #expect(committed)
        #expect(model.mode == .commit)
        #expect(model.committedDiff?.addedCount == 11902)
        #expect(model.confirmationInput.isEmpty)
        let attempts = await port.commitAttempts
        #expect(attempts == ["RESTORE"], "the port is handed the trimmed phrase and checks it too")
    }

    @Test("a refused artifact cannot be committed however perfect the phrase")
    func refusedArtifactCannotBeCommitted() async {
        let port = StubRestorePort(ledgerIsComplete: false)
        let model = Self.model(restore: port)

        await model.runDryRun()
        model.confirmationInput = "RESTORE"

        #expect(model.isRefused)
        #expect(!model.hasCommittableDiff)
        #expect(!model.canCommit)
        let committed = await model.commit()
        #expect(!committed)
        let attempts = await port.commitAttempts
        #expect(attempts.isEmpty)
    }

    @Test("a broken signature chain refuses the restore just as an incomplete ledger does")
    func brokenSignatureChainRefuses() async {
        let model = Self.model(restore: StubRestorePort(signatureChainIsIntact: false))

        await model.runDryRun()

        #expect(model.isRefused)
        #expect(!model.hasCommittableDiff)
    }
}

// MARK: - RestoreShamirTests

/// The default scheme is 2-of-3: any two reconstruct, one alone reveals nothing.
@Suite("Shamir reconstruction needs a quorum of live shares")
@MainActor
struct RestoreShamirTests {
    private static let artifact = URL(fileURLWithPath: "/Backups/capsule-2026-02-22.tar")

    private static func model(
        restore: StubRestorePort = StubRestorePort()
    ) -> RestoreFlowViewModel {
        RestoreFlowViewModel(
            artifact: artifact,
            restore: restore,
            recovery: StubRecoveryPort(restoreSecret: StubRestorePort.reconstructedSecret)
        )
    }

    @Test("shares are listed with their labels, dead ones surfaced rather than hidden")
    func sharesAreListedIncludingDeadOnes() async {
        let model = Self.model()

        await model.loadShares()

        #expect(model.shares.count == 3)
        #expect(model.shares.map(\.label).contains("Safe deposit box"))
        let invalidated = model.shares.filter(\.isInvalidated)
        #expect(invalidated.map(\.id) == ["share-3"], "a user holding a dead share must learn it is dead")
    }

    @Test("the threshold is two, so one share alone reconstructs nothing")
    func oneShareIsNotAQuorum() async {
        let model = Self.model()
        await model.loadShares()

        model.toggleShare("share-1")

        #expect(RestoreFlowViewModel.defaultShamirThreshold == 2)
        #expect(!model.canReconstructFromShares)
        let account = await model.restoreFromSelectedShares()
        #expect(account == nil, "one share must reveal nothing")
    }

    @Test("any two live shares reconstruct the secret and restore the account")
    func twoSharesReconstruct() async {
        let model = Self.model()
        await model.loadShares()

        model.toggleShare("share-1")
        model.toggleShare("share-2")

        #expect(model.canReconstructFromShares)
        let account = await model.restoreFromSelectedShares()
        #expect(account?.handle == "avery@capsule.example")
        #expect(model.state == .ready)
    }

    /// An invalidated share stays *selectable* — it is shown rather than hidden
    /// so a user holding a dead share learns it is dead — but it must not count
    /// towards the quorum. Selecting one live and one dead share therefore
    /// leaves the action disabled, rather than enabling it and failing at the
    /// port in the middle of a recovery.
    @Test("an invalidated share does not count towards the quorum")
    func invalidatedShareCannotComplete() async {
        let model = Self.model()
        await model.loadShares()

        model.toggleShare("share-1")
        model.toggleShare("share-3")

        // Both are selected; only one of them can contribute.
        #expect(model.selectedShareIDs.count == 2)
        #expect(!model.canReconstructFromShares)

        let account = await model.restoreFromSelectedShares()
        #expect(account == nil)
    }

    /// The dead share is offered, not filtered away. Hiding it would leave the
    /// user to discover it is worthless at the moment they need it.
    @Test("an invalidated share is still listed and still selectable")
    func invalidatedShareRemainsVisible() async {
        let model = Self.model()
        await model.loadShares()

        let dead = model.shares.first { $0.isInvalidated }
        #expect(dead != nil)

        model.toggleShare("share-3")
        #expect(model.selectedShareIDs.contains("share-3"))
    }

    @Test("selecting a share twice deselects it")
    func togglingIsIdempotentInPairs() async {
        let model = Self.model()
        await model.loadShares()

        model.toggleShare("share-1")
        model.toggleShare("share-2")
        model.toggleShare("share-2")

        #expect(model.selectedShareIDs == ["share-1"])
        #expect(!model.canReconstructFromShares)
    }
}
