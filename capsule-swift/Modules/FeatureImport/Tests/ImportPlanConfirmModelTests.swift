import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import Testing

@testable import FeatureImport

/// The confirmation screen is the last point at which anything is reversible, so
/// its refusals are what these tests are about.
@Suite("Import plan confirmation")
@MainActor
struct ImportPlanConfirmModelTests {
    private static let gigabyte: UInt64 = 1073741824

    private func model(
        plan: ImportPlan = StubFixtures.plan(),
        availableDiskBytes: UInt64? = 500 * ImportPlanConfirmModelTests.gigabyte,
        planError: (any Error)? = nil
    ) -> ImportPlanConfirmModel {
        ImportPlanConfirmModel(
            scan: StubFixtures.scan(candidateCount: 4),
            importing: StubImportPort(plan: plan, planError: planError),
            storage: StubStoragePort(availableDiskBytes: availableDiskBytes),
            albums: MockEnvironment(scenario: .healthy).albums,
            connectivity: StubFixtures.connectivity
        )
    }

    @Test("the three tiles split the plan exactly")
    func tilesPartitionThePlan() async {
        let confirm = model(plan: StubFixtures.plan(importing: 9, skipping: 3, conflicts: [StubFixtures.conflict(0)]))

        await confirm.load()

        #expect(confirm.count(for: .add) == 9)
        #expect(confirm.count(for: .skip) == 3)
        #expect(confirm.count(for: .conflicts) == 1)
        #expect(confirm.totalCount == 12)
        #expect(confirm.decisions(for: .add).count == 9)
        #expect(confirm.decisions(for: .skip).count == 3)
    }

    /// Skipped candidates contribute no bytes: the meter measures what will be
    /// written, not what was looked at.
    @Test("the summary counts only the bytes that will be written")
    func summaryCountsImportedBytesOnly() async {
        let confirm = model(plan: StubFixtures.plan(importing: 4, skipping: 6, byteSize: 2000000))

        await confirm.load()

        #expect(confirm.totalBytes == 8000000)
    }

    /// "Why did those land there" has to be answerable on this screen.
    @Test("the destination always arrives with the rule that chose it")
    func destinationCarriesItsRule() async {
        let confirm = model(plan: StubFixtures.plan(rule: .scopeOverride))

        await confirm.load()

        #expect(confirm.destinationRule == .scopeOverride)
        #expect(confirm.destinationRule?.reasonKey == "ios.import.plan.reason.scope_override")
    }

    @Test("every resolution rule has a reason sentence")
    func everyRuleExplainsItself() {
        let rules: [ImportPlan.DestinationRule] = [
            .explicitUserPick, .scopeOverride, .sourceKindDefault, .ownerDefaultPointer, .derivedDefaultAlbum,
        ]

        #expect(Set(rules.map(\.reasonKey)).count == rules.count)
        #expect(rules.allSatisfy { $0.reasonKey.hasPrefix("ios.import.plan.reason.") })
    }

    /// A conflict is a decision, not a dead end: answering it must unlock the
    /// confirm rather than merely removing a warning.
    @Test("an unanswered conflict blocks confirm and answering it unblocks")
    func conflictsGateConfirm() async {
        let unresolvable = ImportConflict(
            candidateID: "candidate-0",
            locator: "photokit://camera-roll/IMG_0.HEIC",
            kind: .existingIsEdited,
            resolution: .replaceExisting
        )
        let confirm = model(plan: StubFixtures.plan(conflicts: [unresolvable]))

        await confirm.load()
        #expect(!confirm.canConfirm)
        #expect(confirm.confirm() == nil)

        confirm.resolve("candidate-0", as: .keepBoth)

        #expect(confirm.canConfirm)
        #expect(confirm.confirm()?.conflicts.first?.resolution == .keepBoth)
    }

    @Test("a conflict defaults to a non-destructive resolution")
    func conflictDefaultsAreSafe() {
        for kind in ImportConflictKind.knownCases {
            #expect(!kind.defaultResolution.isDestructive)
            #expect(kind.allowedResolutions.contains(kind.defaultResolution))
        }
    }

    @Test("a plan that does not fit cannot be confirmed")
    func insufficientSpaceBlocksConfirm() async {
        let confirm = model(
            plan: StubFixtures.plan(importing: 4, byteSize: 40 * Self.gigabyte),
            availableDiskBytes: 10 * Self.gigabyte
        )

        await confirm.load()

        #expect(confirm.outlook.state == .insufficient)
        #expect(!confirm.canConfirm)
    }

    @Test("a plan that only just fits recommends streaming but still confirms")
    func tightSpaceStillConfirms() async {
        let available = 20 * Self.gigabyte
        let required = (available - ImportSpaceOutlook.defaultReserveBytes) / 2 + Self.gigabyte
        let confirm = model(
            plan: StubFixtures.plan(importing: 1, skipping: 0, byteSize: required),
            availableDiskBytes: available
        )

        await confirm.load()

        #expect(confirm.outlook.state == .streamingRecommended)
        #expect(confirm.canConfirm)
    }

    @Test("an unmeasurable disk does not block confirm")
    func unknownDiskDoesNotBlock() async {
        let confirm = model(availableDiskBytes: nil)

        await confirm.load()

        #expect(confirm.outlook.state == .comfortable)
        #expect(confirm.canConfirm)
    }

    /// The planner rejects the combination outright; a client that presented it
    /// would be showing a plan the executor will refuse.
    @Test("a staged streaming plan is never confirmable")
    func stagedStreamingIsRefused() async {
        let confirm = model(plan: StubFixtures.plan(streaming: true, uploadPolicy: .staged))

        await confirm.load()

        #expect(confirm.confirm() == nil)
    }

    @Test("a plan with nothing to import is empty, not ready")
    func nothingToImportIsEmpty() async {
        let confirm = model(plan: StubFixtures.plan(importing: 0, skipping: 0))

        await confirm.load()

        #expect(confirm.phase == .empty)
        #expect(!confirm.canConfirm)
    }

    @Test("a planner refusal becomes a failed phase carrying the code")
    func plannerRefusalIsReported() async {
        let confirm = model(planError: CapsuleError(code: .uploadInvalidAction, detail: "refused"))

        await confirm.load()

        #expect(confirm.phase == .failed(.uploadInvalidAction))
        #expect(!confirm.canConfirm)
    }

    @Test("a move plan says it will release the source")
    func moveIsAnnounced() async {
        let confirm = model(plan: StubFixtures.plan(mode: .move))

        await confirm.load()

        #expect(confirm.releasesSource)
    }
}
