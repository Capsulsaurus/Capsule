import CapsuleDomain
import Foundation

// MARK: - RecoverySnooze

/// The two snooze lengths *Backup & Recovery — Schedule and Triggers* offers.
public enum RecoverySnooze: String, Sendable, Hashable, CaseIterable {
    case oneDay = "one_day"
    case oneWeek = "one_week"

    public var days: Int {
        switch self {
        case .oneDay: 1
        case .oneWeek: 7
        }
    }

    /// The catalog key for this option's button.
    public var titleKey: String { "app.recovery.verify.snooze.\(rawValue)" }
}

// MARK: - RecoveryCadence

/// The verification schedule, as pure functions over
/// ``RecoveryVerificationState``.
///
/// Pure and free-standing on purpose: the schedule is a documented contract
/// (7 days → 90 → a 180-day cap after two consecutive successes; re-arm to 7 on
/// enrollment, rotation, or restore; at most three consecutive snoozes, then a
/// persistent badge) and a contract is worth testing under a mocked clock
/// without a view model, a port, or a network in the way.
///
/// The one property that is not a schedule at all, and the reason this type
/// states it out loud: the check **never blocks**. It is advisory by design, so
/// there is no function here that returns "you may not proceed", and
/// ``isBlocking`` is a constant `false` that a test can assert on.
public enum RecoveryCadence {
    /// Whether the cadence may ever gate sync, unlock, or any other flow.
    ///
    /// Always `false`. Present as a value rather than as a comment so the
    /// contract is assertable: *Backup & Recovery* says a UI that gates anything
    /// on this has misread it.
    public static let isBlocking = false

    /// The state after a verification attempt.
    ///
    /// A success advances the backoff ladder and clears the snooze count; a
    /// mismatch advances the failure count and resets the success streak but
    /// **does not** move the due date — a user who just failed should be asked
    /// again, not put off for 90 days.
    ///
    /// An inconclusive outcome (the escrow could not be read) changes nothing
    /// at all: recording it as a failure would punish the user for a network
    /// problem.
    public static func advanced(
        _ state: RecoveryVerificationState,
        after outcome: RecoveryVerificationOutcome,
        now: CapsuleTimestamp
    ) -> RecoveryVerificationState {
        var next = state
        switch outcome {
        case .verified:
            let interval = state.nextIntervalAfterSuccess
            next.consecutiveSuccesses = state.consecutiveSuccesses + 1
            next.consecutiveFailures = 0
            next.snoozeCount = 0
            next.currentIntervalDays = interval
            next.nextDueAt = offset(now, days: interval)
        case .mismatch:
            next.consecutiveFailures = state.consecutiveFailures + 1
            next.consecutiveSuccesses = 0
        case .inconclusive:
            break
        }
        return next
    }

    /// The state after a snooze.
    ///
    /// The count keeps climbing past the cap so the badge stays persistent;
    /// ``RecoveryVerificationState/canSnooze`` is what stops offering the
    /// button. Clamping the count instead would let a user snooze forever, one
    /// snooze at a time.
    public static func snoozed(
        _ state: RecoveryVerificationState,
        by snooze: RecoverySnooze,
        now: CapsuleTimestamp
    ) -> RecoveryVerificationState {
        var next = state
        next.snoozeCount = state.snoozeCount + 1
        next.nextDueAt = offset(now, days: snooze.days)
        return next
    }

    /// The state after a re-arm trigger: a new device enrolls, the secret
    /// rotates, or a restore-from-escrow completes.
    ///
    /// Back to the 7-day step, snoozes and failures cleared. The prompt lands on
    /// the **new** device because that device has never seen the passphrase —
    /// which is exactly the case the 7-day step was chosen to catch.
    public static func rearmed(now: CapsuleTimestamp) -> RecoveryVerificationState {
        RecoveryVerificationState(
            nextDueAt: offset(now, days: RecoveryVerificationState.initialIntervalDays),
            currentIntervalDays: RecoveryVerificationState.initialIntervalDays
        )
    }

    /// Whether a persistent, non-blocking badge should be shown instead of a
    /// prompt: the user has spent every snooze.
    public static func showsPersistentBadge(_ state: RecoveryVerificationState) -> Bool {
        !state.canSnooze
    }

    private static func offset(_ now: CapsuleTimestamp, days: Int) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: now.epochSeconds + Int64(days) * 86400)
    }
}
