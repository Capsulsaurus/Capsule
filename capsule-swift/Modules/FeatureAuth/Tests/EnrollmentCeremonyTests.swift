import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

// MARK: - EnrollmentRailTests

/// The rail is a **named** sequence, and the name of every step is the contract.
@Suite("The first-device rail names six steps and skips none")
@MainActor
struct EnrollmentRailTests {
    @Test("the rail starts as the six documented stages, in order, all pending")
    func railStartsPending() {
        let model = EnrollmentCeremonyViewModel(enrollment: PreviewEnrollmentCeremony())

        #expect(model.rows.map(\.stage) == EnrollmentStage.allCases)
        #expect(model.rows.allSatisfy { $0.status == .pending })
        #expect(model.activeStage == nil)
        #expect(model.failure == nil)
        #expect(!model.isComplete)
    }

    @Test("the documented order is master key → identity → device keys → publish → album → phrase")
    func stageOrderIsTheDocumentedOne() {
        #expect(EnrollmentStage.allCases == [
            .masterKey,
            .userIdentityKey,
            .deviceKeys,
            .publishDirectory,
            .defaultAlbum,
            .recoveryPassphrase,
        ])
    }

    @Test("asking what the device can do happens before the rail claims anything")
    func availabilityIsAskedUpFront() async {
        let softwareOnly = PreviewEnrollmentCeremony(
            behaviour: PreviewCeremonyBehaviour(availability: .softwareOnly)
        )
        let model = EnrollmentCeremonyViewModel(enrollment: softwareOnly)

        #expect(model.hardwareAvailability == .softwareOnly)
        #expect(model.state == .idle)

        await model.prepare()

        #expect(model.state == .ready)
        #expect(!model.usesSecureElement)
    }

    @Test("a healthy ceremony leaves every stage terminal and the account usable")
    func healthyCeremonyCompletes() async {
        let model = EnrollmentCeremonyViewModel(enrollment: PreviewEnrollmentCeremony())
        await model.prepare()

        await model.start()

        #expect(model.rows.allSatisfy { $0.status == .done })
        #expect(model.isComplete)
        #expect(model.failure == nil)
        #expect(!model.isRunning)
        #expect(model.usesSecureElement)
    }

    /// The load-bearing one: completion is "every stage reached a terminal
    /// state", so a ceremony that never reported a step cannot claim to be done.
    @Test("a ceremony that never reports a stage is not complete")
    func skippedStageBlocksCompletion() async {
        let skipped = EnrollmentStage.recoveryPassphrase
        let script = EnrollmentStage.allCases
            .filter { $0 != skipped }
            .map { EnrollmentStageEvent(stage: $0, status: .done) }
        let model = EnrollmentCeremonyViewModel(
            enrollment: ScriptedEnrollmentCeremony(script: script)
        )

        await model.start()

        let unreported = model.rows.first { $0.stage == skipped }
        #expect(unreported?.status == .pending)
        #expect(!model.isComplete)
    }

    @Test("every stage carries a catalog key and a symbol, never display text")
    func stagesCarryCatalogKeys() {
        for stage in EnrollmentStage.allCases {
            #expect(stage.titleKey == "ios.enrollment.stage.\(stage.rawValue).title")
            #expect(stage.explanationKey == "ios.enrollment.stage.\(stage.rawValue).explanation")
            #expect(!stage.titleKey.contains(" "))
            #expect(!stage.symbolName.isEmpty)
        }
    }
}

// MARK: - EnrollmentFailureTests

/// The two failure shapes that must not be flattened into one another: a
/// hardware refusal, which is actionable, and a deferral, which is not a
/// failure at all.
@Suite("A hardware refusal is actionable; a deferral is not a failure")
@MainActor
struct EnrollmentFailureTests {
    @Test("a secure-element refusal stops at the step that failed and leaves the rest untouched")
    func hardwareRefusalStopsAtItsStage() async {
        let model = EnrollmentCeremonyViewModel(
            enrollment: PreviewEnrollmentCeremony(behaviour: .hardwareRefusal)
        )
        await model.prepare()

        await model.start()

        #expect(model.failure == .hardwareKeyUnavailable)
        #expect(!model.isComplete)
        let failedStage = model.rows.first { $0.status == .failed(.hardwareKeyUnavailable) }
        #expect(failedStage?.stage == .deviceKeys)
        let later = model.rows.filter { $0.stage == .publishDirectory || $0.stage == .defaultAlbum }
        #expect(later.allSatisfy { $0.status == .pending })
    }

    @Test("the refusal offers both a retry and the software-key deviation")
    func refusalOffersBothRecoveries() async {
        let model = EnrollmentCeremonyViewModel(
            enrollment: PreviewEnrollmentCeremony(behaviour: .hardwareRefusal)
        )

        await model.start()

        #expect(model.offersSoftwareKeyDeviation)
        #expect(EnrollmentStageFailure.hardwareKeyUnavailable.offersSoftwareKeyDeviation)
        #expect(!EnrollmentStageFailure.cancelled.offersSoftwareKeyDeviation)
    }

    @Test("retrying runs the same ceremony unchanged and records no deviation")
    func retryDoesNotWeakenKeyCustody() async {
        let model = EnrollmentCeremonyViewModel(
            enrollment: PreviewEnrollmentCeremony(behaviour: .hardwareRefusal)
        )
        await model.prepare()
        await model.start()

        await model.retry()

        #expect(model.failure == .hardwareKeyUnavailable)
        #expect(!model.acceptedSoftwareKeyDeviation, "a retry must not quietly become the deviation")
        #expect(model.usesSecureElement, "the device still claims the enclave it has not given up on")
        #expect(model.offersSoftwareKeyDeviation)
    }

    /// The deviation must be *taken on the record*: the summary screen and the
    /// device row in Settings both read this flag afterwards.
    @Test("continuing with software keys records the deviation rather than taking it silently")
    func softwareKeyDeviationIsRecorded() async {
        let model = EnrollmentCeremonyViewModel(
            enrollment: PreviewEnrollmentCeremony(behaviour: .hardwareRefusal)
        )
        await model.prepare()
        await model.start()
        #expect(!model.acceptedSoftwareKeyDeviation)
        #expect(model.usesSecureElement)

        await model.continueWithSoftwareKeys()

        #expect(model.acceptedSoftwareKeyDeviation)
        #expect(model.isComplete)
        #expect(model.failure == nil)
        #expect(model.hardwareAvailability == .secureElement)
        #expect(!model.usesSecureElement, "a device that accepted software keys must stop claiming an enclave")
    }

    @Test("a deferred upload or album finishes setup instead of blocking it")
    func deferralsDoNotBlockSetup() async {
        let model = EnrollmentCeremonyViewModel(
            enrollment: PreviewEnrollmentCeremony(behaviour: .serverUnreachable)
        )

        await model.start()

        #expect(model.deferredStages == [.publishDirectory, .defaultAlbum])
        #expect(model.failure == nil)
        #expect(model.isComplete, "a deferred stage must not strand the user behind the ceremony")
        #expect(model.state == .ready)
    }

    @Test("a deferred status is terminal and carries the reason as a catalog key")
    func deferredStatusIsTerminal() async {
        let model = EnrollmentCeremonyViewModel(
            enrollment: PreviewEnrollmentCeremony(behaviour: .serverUnreachable)
        )

        await model.start()

        let deferred = model.rows.first { $0.stage == .publishDirectory }
        #expect(deferred?.status == .deferred(reasonKey: "ios.enrollment.deferred.directory"))
        #expect(deferred?.status.isTerminal == true)
        #expect(EnrollmentStageStatus.pending.isTerminal == false)
        #expect(EnrollmentStageStatus.running.isTerminal == false)
    }

    @Test("cancelling abandons the ceremony and persists nothing")
    func cancelResetsTheRail() async {
        let port = ScriptedEnrollmentCeremony(
            script: EnrollmentStage.allCases.map { EnrollmentStageEvent(stage: $0, status: .done) }
        )
        let model = EnrollmentCeremonyViewModel(enrollment: port)
        await model.start()
        #expect(model.isComplete)

        await model.cancel()

        #expect(model.state == .idle)
        #expect(model.rows.allSatisfy { $0.status == .pending })
        #expect(!model.isComplete)
        let cancels = await port.cancelCount
        #expect(cancels == 1)
    }
}
