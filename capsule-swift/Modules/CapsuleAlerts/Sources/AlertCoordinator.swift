import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - AlertRefreshOutcome

/// What one pass of the alert loop did — for logs and for tests.
public struct AlertRefreshOutcome: Sendable, Hashable {
    /// How the alarms were delivered this pass.
    public enum Delivery: Sendable, Hashable {
        /// Nothing needed arming, so permission was not consulted.
        case nothingToArm
        /// Permission is granted; the alarms were reconciled.
        case armed
        /// Permission is refused: every class is its in-app badge, and every
        /// armed alarm was cancelled. Not an error.
        case badgesOnly
        /// The predicate refused its input; the armed alarms were left as they
        /// were, since they were right for the last state that parsed.
        case policyFailed
    }

    public var delivery: Delivery
    /// Every alert true now — the in-app badge set.
    public var badges: [Alert]
    /// The changes made to the armed alarms.
    public var operations: [AlertTimerOperation]
}

// MARK: - AlertCoordinator

/// The client half of local alerts: the arm / re-arm / cancel loop, the
/// permission prompt at first use, and the bounded snooze.
///
/// ## One loop
///
/// ``refresh()`` is the only entry point, and it is idempotent: read the
/// facts, ask the predicate which classes are true (the badges) and when each
/// pre-armable class must fire (`pre_arm_deadlines`, per class), then
/// reconcile the OS's alarms against that with ``AlertTimerPlan``. Call it
/// after **any** state change — launch, foreground, a completed sync, a quota
/// read, a snooze — and the alarms follow.
///
/// ## Permission at first use
///
/// The prompt is requested the first time a pass has something to arm, so it
/// arrives with context, and never at launch for its own sake. A refusal is
/// not an error and is never retried: every class degrades to its in-app
/// badge, which ``badges`` still reports.
///
/// ## Disabling suppresses the warning, never the behavior
///
/// Nothing here reaches sync, upload, or any other flow. The coordinator only
/// decides what to *say*.
public actor AlertCoordinator {
    private let facts: any AlertFactsPort
    private let policy: any AlertPolicyPort
    private let scheduler: any AlertSchedulerPort
    private let authorization: any NotificationAuthorizationPort
    private let snoozes: any AlertSnoozeStore
    private let clock: AlertClock

    /// Every alert true at the last refresh — the in-app badge set.
    public private(set) var badges: [Alert] = []

    public init(
        facts: any AlertFactsPort,
        policy: any AlertPolicyPort,
        scheduler: any AlertSchedulerPort,
        authorization: any NotificationAuthorizationPort,
        snoozes: any AlertSnoozeStore,
        clock: AlertClock = .system
    ) {
        self.facts = facts
        self.policy = policy
        self.scheduler = scheduler
        self.authorization = authorization
        self.snoozes = snoozes
        self.clock = clock
    }

    /// Recompute every alert and reconcile the armed alarms.
    @discardableResult
    public func refresh() async -> AlertRefreshOutcome {
        let signpost = CapsuleSignpost.alerts.beginInterval("alerts.refresh")
        defer { CapsuleSignpost.alerts.endInterval("alerts.refresh", signpost) }
        let now = clock.now()
        let raw = await facts.facts()
        var ledger = snoozes.load()

        let decision: Decision
        do {
            decision = try decide(raw, ledger: ledger, now: now)
        } catch {
            CapsuleLog.alerts.error("alert predicate refused its input: \(String(describing: error), privacy: .public)")
            return AlertRefreshOutcome(delivery: .policyFailed, badges: badges, operations: [])
        }
        badges = decision.reported
        if ledger.clear(keeping: decision.stillTrue) { snoozes.save(ledger) }

        let badgeOnly = ledger.badgeOnly
        let desired = decision.deadlines
            .filter { !badgeOnly.contains($0.alertClass) }
            .map { ArmedAlarm(alert: decision.input.armedAlert(for: $0), fireAt: $0.deadline) }
        let trueNames = decision.reported.map(\.alertClass.rawValue).joined(separator: ",")
        let armNames = desired.map { "\($0.alertClass.rawValue)@\($0.fireAt.rfc3339)" }.joined(separator: ",")
        let badgeOnlyNames = badgeOnly.map(\.rawValue).sorted().joined(separator: ",")
        CapsuleLog.alerts.debug(
            "alerts at \(now.rfc3339, privacy: .public): true=[\(trueNames, privacy: .public)] arm=[\(armNames, privacy: .public)] badgeOnly=[\(badgeOnlyNames, privacy: .public)]"
        )

        let (delivery, operations) = await deliver(desired)
        CapsuleLog.alerts.info(
            "alert refresh: delivery=\(String(describing: delivery), privacy: .public) badges=\(decision.reported.count, privacy: .public) operations=\(operations.count, privacy: .public)"
        )
        return AlertRefreshOutcome(delivery: delivery, badges: decision.reported, operations: operations)
    }

    /// What the predicate decided for one pass.
    private struct Decision {
        /// The facts with the ledger's snoozes applied.
        var input: NotifyInput
        /// Every alert true now — the badges.
        var reported: [Alert]
        /// When each pre-armable class must fire.
        var deadlines: [ClassDeadline]
        /// The classes whose condition holds, snoozed or not.
        var stillTrue: Set<AlertClass>
    }

    private func decide(_ raw: NotifyInput, ledger: AlertSnoozeLedger, now: CapsuleTimestamp) throws -> Decision {
        let input = Self.applying(ledger, to: raw)
        // Whether a class is still *true* is asked without this ledger's
        // snoozes: a snoozed class reports nothing, and reading that as
        // "cleared" would hand it a fresh budget every snooze.
        var unsnoozed = input
        unsnoozed.suppressedUntil = raw.suppressedUntil
        return try Decision(
            input: input,
            reported: policy.evaluate(input, at: now),
            deadlines: policy.preArmDeadlines(input, at: now),
            stillTrue: Set(policy.evaluate(unsnoozed, at: now).map(\.alertClass))
        )
    }

    /// Reconcile the OS's alarms against `desired`, prompting for permission
    /// only when there is something to arm.
    private func deliver(_ desired: [ArmedAlarm]) async -> (AlertRefreshOutcome.Delivery, [AlertTimerOperation]) {
        let delivery: AlertRefreshOutcome.Delivery
        let target: [ArmedAlarm]
        if desired.isEmpty {
            delivery = .nothingToArm
            target = []
        } else if await permitted() {
            delivery = .armed
            target = desired
        } else {
            delivery = .badgesOnly
            target = []
        }
        let operations = await AlertTimerPlan.reconcile(desired: target, armed: scheduler.armed())
        for operation in operations {
            await apply(operation)
        }
        return (delivery, operations)
    }

    /// Snooze `alertClass`, then refresh so its alarm moves to the snooze end.
    ///
    /// Bounded: once the budget is spent the class is badge-only and this
    /// returns ``AlertSnoozeOutcome/badgeOnly`` without snoozing again. The
    /// recovery check is snoozed through its own cadence instead.
    @discardableResult
    public func snooze(_ alertClass: AlertClass) async -> AlertSnoozeOutcome {
        var ledger = snoozes.load()
        let outcome = ledger.snooze(alertClass, now: clock.now())
        if case .snoozed = outcome { snoozes.save(ledger) }
        CapsuleLog.alerts.info(
            "snooze \(alertClass.rawValue, privacy: .public): \(String(describing: outcome), privacy: .public)"
        )
        await refresh()
        return outcome
    }

    // MARK: Helpers

    /// The facts with this ledger's snoozes merged in; where the facts already
    /// carry one for the same class (the sync layer's own staleness snooze),
    /// the later instant wins.
    static func applying(_ ledger: AlertSnoozeLedger, to input: NotifyInput) -> NotifyInput {
        var merged = input
        for (alertClass, until) in ledger.suppressions {
            merged.suppressedUntil[alertClass] = max(until, input.suppressedUntil[alertClass] ?? until)
        }
        return merged
    }

    /// Whether alarms may be armed, prompting the first time it matters.
    private func permitted() async -> Bool {
        switch await authorization.status() {
        case .authorized:
            return true
        case .denied:
            return false
        case .notDetermined:
            CapsuleLog.alerts.info("requesting notification permission: a class has something to arm")
            let answer = await authorization.request()
            CapsuleLog.alerts.info("notification permission answered: \(String(describing: answer), privacy: .public)")
            return answer == .authorized
        }
    }

    private func apply(_ operation: AlertTimerOperation) async {
        switch operation {
        case let .cancel(alertClass):
            await scheduler.cancel(alertClass)
            CapsuleLog.alerts.debug("cancelled alarm \(alertClass.rawValue, privacy: .public)")
        case let .arm(alarm):
            do {
                try await scheduler.arm(alarm)
                CapsuleLog.alerts.debug(
                    "armed alarm \(alarm.alertClass.rawValue, privacy: .public) at \(alarm.fireAt.rfc3339, privacy: .public)"
                )
            } catch {
                // The badge still shows; the next refresh tries again.
                CapsuleLog.alerts.error(
                    "arming \(alarm.alertClass.rawValue, privacy: .public) failed: \(String(describing: error), privacy: .public)"
                )
            }
        }
    }
}
