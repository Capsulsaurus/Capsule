import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

// MARK: - RestoreReconciliationRuleTests

/// "You are about to restore" is not informed consent; the four outcomes are.
@Suite("The dry-run report enumerates all four reconciliation outcomes")
struct RestoreReconciliationRuleTests {
    @Test("all four outcomes are named, each by its own pair of catalog keys")
    func everyOutcomeIsNamed() {
        let rules = RestoreReconciliationRule.allCases

        #expect(rules.count == 4)
        #expect(Set(rules.map(\.titleKey)).count == 4)
        #expect(Set(rules.map(\.detailKey)).count == 4)
        for rule in rules {
            #expect(rule.titleKey == "app.settings.recovery.restore.rule.\(rule.rawValue)")
            #expect(rule.detailKey == "app.settings.recovery.restore.outcome.\(rule.rawValue)")
            #expect(!rule.titleKey.contains(" "))
        }
    }

    @Test("the outcomes cover kept, quarantined, applied, and no-op")
    func outcomesCoverEveryDirection() {
        #expect(RestoreReconciliationRule.allCases.contains(.identicalHead))
        #expect(RestoreReconciliationRule.allCases.contains(.localAhead))
        #expect(RestoreReconciliationRule.allCases.contains(.divergent))
        #expect(RestoreReconciliationRule.allCases.contains(.absentLocally))
    }
}

// MARK: - BackupAndRecoverySettingsTests

/// One root — the recovery secret — with wraps under it, rather than a list of
/// independent safety nets that would be false.
@Suite("Backup & Recovery loads, verifies locally, and gates the restore")
@MainActor
struct BackupAndRecoverySettingsTests {
    private static func model(
        _ port: StubRecoveryPort,
        connection: ConnectionClass? = .unmetered
    ) -> BackupAndRecoverySettingsModel {
        BackupAndRecoverySettingsModel(
            recovery: port,
            connectivity: .stub(connection: connection),
            clock: SettingsInstant.clock
        )
    }

    @Test("a configured escrow loads with its cadence")
    func configuredEscrowLoads() async {
        let model = Self.model(StubRecoveryPort())

        await model.load()

        #expect(model.phase == .ready)
        #expect(model.isConfigured)
        #expect(!model.isVerificationDue, "the next prompt is 51 days out")
        #expect(model.canSnooze)
        #expect(!model.shouldOfferGuidedRewrap)
        #expect(model.restoreRules == RestoreReconciliationRule.allCases)
    }

    /// An account without recovery can lose everything to one lost phone, which
    /// is the one thing worth nagging about.
    @Test("an account with no escrow is reported as unconfigured, not as configured-and-empty")
    func unconfiguredEscrowIsVisible() async {
        let model = Self.model(StubRecoveryPort(escrow: StubRecoveryPort.unconfiguredEscrow))

        await model.load()

        #expect(!model.isConfigured)
        #expect(model.summary?.hasServerEscrow == false)
    }

    @Test("an escrow that cannot be read is a failure the screen can classify")
    func unreadableEscrowFails() async {
        let port = StubRecoveryPort(readFailure: StubError.failure(.syncUnauthenticated))
        let model = Self.model(port)

        await model.load()

        #expect(model.phase == .failed(.syncUnauthenticated))
        #expect(model.summary == nil)
        #expect(!model.isConfigured)
    }

    @Test("a failed read while the radio is off reads as offline, not as a server error")
    func offlineBeatsTheCode() async {
        let port = StubRecoveryPort(readFailure: StubError.failure(.syncUnauthenticated))
        let model = Self.model(port, connection: .offline)

        await model.load()

        #expect(model.phase == .offline)
    }

    @Test("an overdue account with its snoozes spent offers the guided re-wrap")
    func overdueAccountOffersRewrap() async {
        let model = Self.model(StubRecoveryPort(escrow: StubRecoveryPort.overdueEscrow))

        await model.load()

        #expect(model.isVerificationDue)
        #expect(!model.canSnooze)
        #expect(model.shouldOfferGuidedRewrap)
    }

    @Test("setting up recovery mints a secret, held only until it is dismissed")
    func mintedSecretIsHeldThenDropped() async {
        let model = Self.model(StubRecoveryPort(escrow: StubRecoveryPort.unconfiguredEscrow))
        await model.load()

        await model.setUpRecovery()

        #expect(model.mintedSecret == StubRecoveryPort.phrase)
        #expect(!model.isWorking)

        model.dismissMintedSecret()

        #expect(model.mintedSecret == nil)
    }

    @Test("a local check records the outcome and forgets what was typed")
    func verificationForgetsTheInput() async {
        let model = Self.model(StubRecoveryPort())
        await model.load()
        model.passphraseInput = StubRecoveryPort.phrase

        await model.verify()

        #expect(model.lastVerification == .verified)
        #expect(model.passphraseInput.isEmpty, "the typed secret must not survive the check")
        #expect(model.summary?.verification.consecutiveSuccesses == 1)
    }

    @Test("a wrong passphrase is a mismatch, and a failure cannot lock anything")
    func wrongPassphraseIsJustAMismatch() async {
        let model = Self.model(StubRecoveryPort())
        await model.load()
        model.passphraseInput = "not-the-phrase"

        await model.verify()

        #expect(model.lastVerification == .mismatch)
        #expect(model.passphraseInput.isEmpty)
        #expect(model.phase == .ready, "a failed check is not an error state")
        #expect(model.summary?.verification.consecutiveFailures == 1)
    }

    @Test("snoozing pushes the prompt out by the requested number of days")
    func snoozeMovesThePromptOnTheInjectedClock() async {
        let port = StubRecoveryPort()
        let model = Self.model(port)
        await model.load()

        await model.snooze(days: 7)

        let recorded = await port.snoozedUntil
        #expect(recorded == [SettingsInstant.days(7)])
        #expect(model.summary?.verification.nextDueAt == SettingsInstant.days(7))
        #expect(model.summary?.verification.snoozeCount == 1)
    }

    /// Wrap rotation, not key rotation: an O(1) escrow replacement with no data
    /// re-encryption, which is why it can be offered casually.
    @Test("the guided rotation mints a fresh secret and re-arms the cadence")
    func rotationRewrapsTheSameMasterKey() async {
        let port = StubRecoveryPort(escrow: StubRecoveryPort.overdueEscrow)
        let model = Self.model(port)
        await model.load()

        await model.rotateSecret()

        #expect(model.mintedSecret == StubRecoveryPort.rotatedPhrase)
        #expect(model.summary?.verification.currentIntervalDays == RecoveryVerificationState.initialIntervalDays)
        #expect(model.canSnooze, "a re-armed cadence has its snoozes back")
        #expect(!model.shouldOfferGuidedRewrap)
        let generation = await port.currentGeneration
        #expect(generation == 1)
    }

    /// The check lives in the model, not only in a disabled button: a disabled
    /// button is a rendering decision and this is a contract.
    @Test("a restore with an unsatisfied phrase gate never reaches the port")
    func restoreIsRefusedWithoutTheTypedPhrase() async {
        let port = StubRecoveryPort()
        let model = Self.model(port)
        await model.load()
        model.restoreSecretInput = StubRecoveryPort.phrase
        let gate = TypedPhraseGate(requiredPhrase: "RESTORE")
        gate.typedPhrase = "restore"

        let ran = await model.commitRestore(gate: gate)

        #expect(!ran)
        let attempts = await port.restoreAttempts
        #expect(attempts.isEmpty, "a refused restore must not be attempted at all")
        #expect(model.restoredAccount == nil)
    }

    @Test("a restore with no secret typed is refused too")
    func restoreNeedsASecret() async {
        let port = StubRecoveryPort()
        let model = Self.model(port)
        await model.load()
        let gate = TypedPhraseGate(requiredPhrase: "RESTORE")
        gate.typedPhrase = "RESTORE"

        let ran = await model.commitRestore(gate: gate)

        #expect(!ran)
        let attempts = await port.restoreAttempts
        #expect(attempts.isEmpty)
    }

    @Test("a satisfied gate and a secret restore the account and clear the field")
    func satisfiedGateRestores() async {
        let port = StubRecoveryPort()
        let model = Self.model(port)
        await model.load()
        model.restoreSecretInput = StubRecoveryPort.phrase
        let gate = TypedPhraseGate(requiredPhrase: "RESTORE")
        gate.typedPhrase = "  RESTORE  "

        let ran = await model.commitRestore(gate: gate)

        #expect(ran)
        #expect(model.restoredAccount?.handle == "avery@capsule.example")
        #expect(model.restoreSecretInput.isEmpty)
        let attempts = await port.restoreAttempts
        #expect(attempts == [StubRecoveryPort.phrase])
    }

    @Test("a secret that does not unwrap the escrow leaves the account untouched")
    func wrongSecretDoesNotRestore() async {
        let model = Self.model(StubRecoveryPort())
        await model.load()
        model.restoreSecretInput = "not-the-phrase"
        let gate = TypedPhraseGate(requiredPhrase: "RESTORE")
        gate.typedPhrase = "RESTORE"

        await model.commitRestore(gate: gate)

        #expect(model.restoredAccount == nil)
        #expect(model.phase == .failed(.escrowMalformed))
    }
}
