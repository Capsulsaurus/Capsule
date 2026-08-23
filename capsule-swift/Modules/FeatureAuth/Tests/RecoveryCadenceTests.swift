import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

// MARK: - RecoveryCadenceTests

/// The schedule is a documented contract, so it is tested as pure arithmetic
/// against an instant the test chooses. Nothing here reads the wall clock.
@Suite("The verification schedule is 7 → 90 → 180, and it never blocks")
struct RecoveryCadenceTests {
    private static let now = AuthInstant.reference

    @Test("the cadence is advisory: nothing here can gate another flow")
    func cadenceNeverBlocks() {
        #expect(RecoveryCadence.isBlocking == false)
    }

    @Test("a re-arm puts the prompt seven days out with a clean slate")
    func rearmReturnsToTheSevenDayStep() {
        let spent = RecoveryVerificationState(
            nextDueAt: AuthInstant.days(-1),
            currentIntervalDays: 180,
            snoozeCount: 3,
            consecutiveFailures: 2,
            consecutiveSuccesses: 5
        )

        let armed = RecoveryCadence.rearmed(now: Self.now)

        #expect(armed.currentIntervalDays == 7)
        #expect(armed.nextDueAt == AuthInstant.days(7))
        #expect(armed.snoozeCount == 0)
        #expect(armed.consecutiveFailures == 0)
        #expect(armed.consecutiveSuccesses == 0)
        #expect(spent.currentIntervalDays == 180, "re-arming builds a new state, it does not mutate the old one")
    }

    @Test("two consecutive successes climb 7 → 90 → 180 and then stay at the cap")
    func successesClimbTheLadderAndStop() {
        var state = RecoveryCadence.rearmed(now: Self.now)

        state = RecoveryCadence.advanced(state, after: .verified, now: Self.now)
        #expect(state.currentIntervalDays == 90)
        #expect(state.nextDueAt == AuthInstant.days(90))

        state = RecoveryCadence.advanced(state, after: .verified, now: Self.now)
        #expect(state.currentIntervalDays == 180)
        #expect(state.nextDueAt == AuthInstant.days(180))

        state = RecoveryCadence.advanced(state, after: .verified, now: Self.now)
        #expect(state.currentIntervalDays == 180, "the cap is a ceiling, not another rung")
        #expect(state.consecutiveSuccesses == 3)
    }

    /// A user who just failed should be asked again, not put off for 90 days.
    @Test("a mismatch counts the failure but does not move the due date")
    func mismatchDoesNotPushTheDueDateOut() {
        let due = AuthInstant.days(7)
        let state = RecoveryVerificationState(
            nextDueAt: due,
            currentIntervalDays: 7,
            consecutiveSuccesses: 1
        )

        let next = RecoveryCadence.advanced(state, after: .mismatch, now: Self.now)

        #expect(next.nextDueAt == due)
        #expect(next.currentIntervalDays == 7)
        #expect(next.consecutiveFailures == 1)
        #expect(next.consecutiveSuccesses == 0, "a mismatch breaks the success streak")
    }

    @Test("three consecutive mismatches route the user to the guided re-wrap")
    func threeFailuresOfferGuidedRewrap() {
        var state = RecoveryCadence.rearmed(now: Self.now)

        for expected in 1 ... 3 {
            state = RecoveryCadence.advanced(state, after: .mismatch, now: Self.now)
            #expect(state.consecutiveFailures == expected)
        }

        #expect(state.shouldOfferGuidedRewrap)
        #expect(RecoveryVerificationState.failuresBeforeGuidedRewrap == 3)
    }

    @Test("two mismatches are not yet a re-wrap, and a success clears the count")
    func successClearsTheFailureCount() {
        var state = RecoveryCadence.rearmed(now: Self.now)
        state = RecoveryCadence.advanced(state, after: .mismatch, now: Self.now)
        state = RecoveryCadence.advanced(state, after: .mismatch, now: Self.now)
        #expect(!state.shouldOfferGuidedRewrap)

        state = RecoveryCadence.advanced(state, after: .verified, now: Self.now)

        #expect(state.consecutiveFailures == 0)
        #expect(!state.shouldOfferGuidedRewrap)
    }

    /// Recording a network problem as a failure would punish the user for it.
    @Test("an inconclusive check changes nothing at all")
    func inconclusiveIsNotAFailure() {
        let state = RecoveryVerificationState(
            nextDueAt: AuthInstant.days(7),
            currentIntervalDays: 7,
            snoozeCount: 2,
            consecutiveFailures: 1,
            consecutiveSuccesses: 1
        )

        let next = RecoveryCadence.advanced(state, after: .inconclusive(.syncCursorInvalid), now: Self.now)

        #expect(next == state)
    }
}

// MARK: - RecoverySnoozeTests

/// Three snoozes, then a badge that cannot be dismissed away.
@Suite("Snoozes are capped at three, then the badge is persistent")
struct RecoverySnoozeTests {
    private static let now = AuthInstant.reference

    @Test("each snooze option pushes the prompt out by its own number of days", arguments: RecoverySnooze.allCases)
    func snoozeMovesTheDueDate(option: RecoverySnooze) {
        let state = RecoveryCadence.rearmed(now: Self.now)

        let snoozed = RecoveryCadence.snoozed(state, by: option, now: Self.now)

        #expect(snoozed.nextDueAt == AuthInstant.days(option.days))
        #expect(snoozed.snoozeCount == 1)
        #expect(option.titleKey == "ios.recovery.verify.snooze.\(option.rawValue)")
    }

    @Test("the snooze lengths are one day and one week")
    func snoozeLengthsAreDocumented() {
        #expect(RecoverySnooze.oneDay.days == 1)
        #expect(RecoverySnooze.oneWeek.days == 7)
        #expect(RecoverySnooze.allCases.count == 2)
    }

    @Test("the third snooze is the last one offered, and the badge then persists")
    func thirdSnoozeExhaustsTheOffer() {
        var state = RecoveryCadence.rearmed(now: Self.now)

        for taken in 1 ... 3 {
            #expect(state.canSnooze, "snooze \(taken) should still be on offer")
            #expect(!RecoveryCadence.showsPersistentBadge(state))
            state = RecoveryCadence.snoozed(state, by: .oneDay, now: Self.now)
        }

        #expect(state.snoozeCount == 3)
        #expect(!state.canSnooze)
        #expect(RecoveryCadence.showsPersistentBadge(state), "a user out of snoozes keeps the badge")
        #expect(RecoveryVerificationState.maximumConsecutiveSnoozes == 3)
    }

    /// Clamping the count would let a user snooze forever, one snooze at a time.
    @Test("the count keeps climbing past the cap, so the badge cannot be reset")
    func countIsNotClampedAtTheCap() {
        var state = RecoveryVerificationState(nextDueAt: AuthInstant.days(1), snoozeCount: 3)

        state = RecoveryCadence.snoozed(state, by: .oneWeek, now: Self.now)

        #expect(state.snoozeCount == 4)
        #expect(RecoveryCadence.showsPersistentBadge(state))
    }

    @Test("a successful verification is what buys the snoozes back")
    func successClearsTheSnoozeCount() {
        let spent = RecoveryVerificationState(
            nextDueAt: AuthInstant.days(-1),
            currentIntervalDays: 7,
            snoozeCount: 3
        )

        let next = RecoveryCadence.advanced(spent, after: .verified, now: Self.now)

        #expect(next.snoozeCount == 0)
        #expect(next.canSnooze)
        #expect(!RecoveryCadence.showsPersistentBadge(next))
    }

    @Test("a prompt is due only once its instant has arrived, and never when unarmed")
    func dueIsMeasuredAgainstTheInjectedInstant() {
        let armed = RecoveryVerificationState(nextDueAt: AuthInstant.days(7))
        let unarmed = RecoveryVerificationState()

        #expect(!armed.isDue(at: AuthInstant.days(6)))
        #expect(armed.isDue(at: AuthInstant.days(7)))
        #expect(armed.isDue(at: AuthInstant.days(8)))
        #expect(!unarmed.isDue(at: AuthInstant.days(3650)), "a sponsored account is never prompted")
    }
}
