import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

// MARK: - RecoveryVerificationTests

/// The periodic prompt, driven by an injected clock. Nothing here reads the
/// wall clock, so "due in 51 days" is an assertion rather than an approximation.
@Suite("The verification prompt is advisory, local, and un-auto-passable")
@MainActor
struct RecoveryVerificationTests {
    private static func model(
        verification: RecoveryVerificationState,
        hasServerEscrow: Bool = true
    ) -> RecoveryVerificationViewModel {
        RecoveryVerificationViewModel(
            recovery: StubRecoveryPort(hasServerEscrow: hasServerEscrow, verification: verification),
            now: AuthInstant.frozen
        )
    }

    @Test("the prompt may never gate another flow")
    func promptNeverBlocks() {
        let model = Self.model(verification: RecoveryVerificationState())

        #expect(!model.blocksOtherFlows)
    }

    @Test("an account with no escrow loads as empty rather than as a prompt")
    func unconfiguredAccountIsEmpty() async {
        let model = Self.model(verification: RecoveryVerificationState(), hasServerEscrow: false)

        await model.load()

        #expect(model.state == .empty)
        #expect(!model.isArmed, "an account with nothing to verify must not be prompted")
        #expect(!model.isDue)
    }

    @Test("a due date in the past is due; one in the future is not")
    func dueIsMeasuredAgainstTheInjectedClock() async {
        let overdue = Self.model(verification: RecoveryVerificationState(nextDueAt: AuthInstant.days(-1)))
        let upcoming = Self.model(verification: RecoveryVerificationState(nextDueAt: AuthInstant.days(51)))

        await overdue.load()
        await upcoming.load()

        #expect(overdue.state == .ready)
        #expect(overdue.isArmed)
        #expect(overdue.isDue)
        #expect(upcoming.isArmed)
        #expect(!upcoming.isDue)
        #expect(upcoming.currentIntervalDays == 7)
    }

    @Test("a correct passphrase advances the ladder and forgets what was typed")
    func successAdvancesAndForgets() async {
        let model = Self.model(verification: RecoveryVerificationState(nextDueAt: AuthInstant.days(-1)))
        await model.load()
        model.passphraseInput = StubRecoveryPort.phrase

        await model.verify()

        #expect(model.lastOutcome == .verified)
        #expect(model.passphraseInput.isEmpty, "the typed secret must not survive the check")
        #expect(model.currentIntervalDays == 90)
        #expect(model.verification.nextDueAt == AuthInstant.days(90))
        #expect(!model.isDue)
    }

    @Test("a wrong passphrase is forgotten too, and does not push the prompt out")
    func failureAlsoForgetsAndDoesNotDefer() async {
        let due = AuthInstant.days(-1)
        let model = Self.model(verification: RecoveryVerificationState(nextDueAt: due))
        await model.load()
        model.passphraseInput = "not-the-phrase"

        await model.verify()

        #expect(model.lastOutcome == .mismatch)
        #expect(model.passphraseInput.isEmpty)
        #expect(model.verification.nextDueAt == due, "a failed check asks again, it does not wait 90 days")
        #expect(model.verification.consecutiveFailures == 1)
        #expect(!model.offersGuidedRewrap)
    }

    @Test("a blank entry is not submitted as a guess")
    func blankInputIsNotAGuess() async {
        let port = StubRecoveryPort(verification: RecoveryVerificationState(nextDueAt: AuthInstant.days(-1)))
        let model = RecoveryVerificationViewModel(recovery: port, now: AuthInstant.frozen)
        await model.load()
        model.passphraseInput = "   \n "

        await model.verify()

        let attempts = await port.verifiedPassphrases
        #expect(attempts.isEmpty)
        #expect(model.lastOutcome == nil)
        #expect(model.passphraseInput.isEmpty)
    }

    @Test("three failed checks route the user to the guided re-wrap")
    func threeFailuresOfferGuidedRewrap() async {
        let model = Self.model(verification: RecoveryVerificationState(nextDueAt: AuthInstant.days(-1)))
        await model.load()

        for attempt in 1 ... 3 {
            model.passphraseInput = "wrong-\(attempt)"
            await model.verify()
        }

        #expect(model.verification.consecutiveFailures == 3)
        #expect(model.offersGuidedRewrap)
    }

    @Test("declaring the secret lost goes straight to the re-wrap, no failures required")
    func declaringLossOffersRewrapImmediately() async {
        let model = Self.model(verification: RecoveryVerificationState(nextDueAt: AuthInstant.days(-1)))
        await model.load()
        #expect(!model.offersGuidedRewrap)

        model.declareSecretLost()

        #expect(model.offersGuidedRewrap)
        #expect(model.verification.consecutiveFailures == 0)
    }

    @Test("snoozing is offered three times and then refused")
    func snoozesAreCappedAtThree() async {
        let port = StubRecoveryPort(verification: RecoveryVerificationState(nextDueAt: AuthInstant.days(-1)))
        let model = RecoveryVerificationViewModel(recovery: port, now: AuthInstant.frozen)
        await model.load()

        for taken in 1 ... 3 {
            #expect(model.canSnooze, "snooze \(taken) should be on offer")
            let accepted = await model.snooze(.oneDay)
            #expect(accepted)
        }

        #expect(!model.canSnooze)
        #expect(model.showsPersistentBadge, "an exhausted snooze budget leaves a badge that stays")
        let refused = await model.snooze(.oneWeek)
        #expect(refused == false)
        let recorded = await port.snoozedUntil
        #expect(recorded.count == 3, "a refused snooze must not reach the port")
    }

    @Test("a snooze moves the prompt by the option's own length")
    func snoozeUsesTheChosenLength() async {
        let model = Self.model(verification: RecoveryVerificationState(nextDueAt: AuthInstant.days(-1)))
        await model.load()

        let accepted = await model.snooze(.oneWeek)

        #expect(accepted)
        #expect(model.verification.nextDueAt == AuthInstant.days(7))
        #expect(model.verification.snoozeCount == 1)
        #expect(!model.showsPersistentBadge)
    }

    @Test("re-arming after an enrollment puts the prompt back on the seven-day step")
    func rearmingResetsTheCadence() async {
        let spent = RecoveryVerificationState(
            nextDueAt: AuthInstant.days(-1),
            currentIntervalDays: 180,
            snoozeCount: 3,
            consecutiveFailures: 3
        )
        let model = Self.model(verification: spent)
        await model.load()
        #expect(model.offersGuidedRewrap)
        #expect(model.showsPersistentBadge)

        model.rearmAfterEnrollment()

        #expect(model.currentIntervalDays == 7)
        #expect(model.verification.nextDueAt == AuthInstant.days(7))
        #expect(model.canSnooze)
        #expect(!model.offersGuidedRewrap)
        #expect(model.lastOutcome == nil)
    }
}
