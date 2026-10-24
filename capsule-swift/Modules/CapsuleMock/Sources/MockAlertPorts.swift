import CapsuleDomain
import CapsulePorts
import Foundation

// MARK: - MockAlertPolicy

/// The mock lane's stand-in for `capsule-core::notify`.
///
/// The mock lane links no Rust, so something has to answer the predicate.
/// This follows the core's documented rules — thresholds fire at the boundary
/// instant, a snooze is exclusive and **defers** a pre-armed deadline, a
/// disable cancels it, and only `sync_stale` and `recovery_check_due` are ever
/// armed — and nothing outside the mock lane may compose it: the decision
/// function is the core's, and the real adapter calls it.
public struct MockAlertPolicy: AlertPolicyPort {
    /// The staleness threshold, two weeks (*Download & Sync — Notifications*).
    public static let syncStaleSeconds: Int64 = .init(SyncStatus.stalenessThresholdDays) * 86400

    public init() {}

    public func evaluate(_ input: NotifyInput, at now: CapsuleTimestamp) throws -> [Alert] {
        AlertClass.allCases.compactMap { alertClass in
            if input.disabled.contains(alertClass) { return nil }
            if let until = input.suppressedUntil[alertClass], until > now { return nil }
            return alert(alertClass, input: input, now: now)
        }
    }

    public func preArmDeadlines(_ input: NotifyInput, at now: CapsuleTimestamp) throws -> [ClassDeadline] {
        AlertClass.allCases.compactMap { alertClass in
            guard alertClass.isPreArmable,
                  !input.disabled.contains(alertClass),
                  var instant = conditionInstant(alertClass, input: input)
            else { return nil }
            if let until = input.suppressedUntil[alertClass] { instant = max(instant, until) }
            return instant > now ? ClassDeadline(alertClass: alertClass, deadline: instant) : nil
        }
    }

    private func alert(_ alertClass: AlertClass, input: NotifyInput, now: CapsuleTimestamp) -> Alert? {
        switch alertClass {
        case .syncStale:
            syncStale(input, now: now)
        case .recoveryCheckDue:
            recoveryCheckDue(input, now: now)
        case .quotaSoft:
            input.quotaState == .softWarning ? Alert(alertClass: alertClass) : nil
        case .quotaGraceExpiring:
            quotaGraceExpiring(input.quotaState)
        case .quarantinePending:
            counted(alertClass, input.quarantinePending)
        case .dropPending:
            counted(alertClass, input.dropsPending)
        }
    }

    private func syncStale(_ input: NotifyInput, now: CapsuleTimestamp) -> Alert? {
        guard let last = input.lastCompletedSync, input.unsyncedChanges > 0 else { return nil }
        let deadline = CapsuleTimestamp(epochSeconds: last.epochSeconds + Self.syncStaleSeconds)
        guard now >= deadline else { return nil }
        return Alert(alertClass: .syncStale, deadline: deadline, params: [
            Alert.Parameter.count: String(input.unsyncedChanges),
            Alert.Parameter.daysBehind: String((now.epochSeconds - last.epochSeconds) / 86400),
        ])
    }

    private func recoveryCheckDue(_ input: NotifyInput, now: CapsuleTimestamp) -> Alert? {
        guard let due = input.recoveryNextDue else { return nil }
        if let until = input.recoverySnoozedUntil, until > now { return nil }
        guard now >= due else { return nil }
        return Alert(
            alertClass: .recoveryCheckDue,
            deadline: max(due, input.recoverySnoozedUntil ?? due),
            params: [
                Alert.Parameter.snoozeBudget: input.recoverySnoozeBudgetSpent ? "spent" : "available",
                Alert.Parameter.recovery: input.recoveryRewrapDue ? "rewrap" : "check",
            ]
        )
    }

    private func quotaGraceExpiring(_ state: QuotaState?) -> Alert? {
        switch state {
        case .hardExceeded: Alert(alertClass: .quotaGraceExpiring, params: [Alert.Parameter.grace: "counting"])
        case .graceExpired: Alert(alertClass: .quotaGraceExpiring, params: [Alert.Parameter.grace: "expired"])
        default: nil
        }
    }

    private func counted(_ alertClass: AlertClass, _ pending: Int) -> Alert? {
        pending > 0 ? Alert(alertClass: alertClass, params: [Alert.Parameter.count: String(pending)]) : nil
    }

    private func conditionInstant(_ alertClass: AlertClass, input: NotifyInput) -> CapsuleTimestamp? {
        switch alertClass {
        case .syncStale:
            guard let last = input.lastCompletedSync, input.unsyncedChanges > 0 else { return nil }
            return CapsuleTimestamp(epochSeconds: last.epochSeconds + Self.syncStaleSeconds)
        case .recoveryCheckDue:
            guard let due = input.recoveryNextDue, !input.recoverySnoozeBudgetSpent else { return nil }
            return max(due, input.recoverySnoozedUntil ?? due)
        case .quotaSoft, .quotaGraceExpiring, .quarantinePending, .dropPending:
            return nil
        }
    }
}

// MARK: - InMemoryAlertScheduler

/// An OS notification scheduler that schedules nothing: it records what is
/// armed, one alarm per class, so the mock lane and the tests can read it
/// back. The mock lane composes this rather than `UNUserNotificationCenter`
/// for the reason it composes `MockLocalAuthenticator`: a system prompt over
/// a mocked world is exactly what that composition root keeps out.
public actor InMemoryAlertScheduler: AlertSchedulerPort {
    private var alarms: [AlertClass: ArmedAlarm]
    /// Every arm and cancel, in order — what a lifecycle test asserts on.
    public private(set) var log: [Event] = []

    public enum Event: Sendable, Hashable {
        case armed(ArmedAlarm)
        case cancelled(AlertClass)
    }

    public init(armed: [ArmedAlarm] = []) {
        alarms = Dictionary(armed.map { ($0.alertClass, $0) }, uniquingKeysWith: { _, last in last })
    }

    public func armed() async -> [AlertClass: ArmedAlarm] {
        alarms
    }

    public func arm(_ alarm: ArmedAlarm) async throws {
        alarms[alarm.alertClass] = alarm
        log.append(.armed(alarm))
    }

    public func cancel(_ alertClass: AlertClass) async {
        guard alarms.removeValue(forKey: alertClass) != nil else { return }
        log.append(.cancelled(alertClass))
    }
}

// MARK: - MockNotificationAuthorization

/// A notification-permission prompt that answers from a script instead of
/// presenting one.
public actor MockNotificationAuthorization: NotificationAuthorizationPort {
    private var current: NotificationAuthorization
    private let answer: NotificationAuthorization
    /// How many times the prompt was requested.
    public private(set) var requestCount = 0

    /// - Parameters:
    ///   - status: the authorization before anyone asks.
    ///   - answer: what the user chooses when asked.
    public init(status: NotificationAuthorization = .notDetermined, answer: NotificationAuthorization = .authorized) {
        current = status
        self.answer = answer
    }

    public func status() async -> NotificationAuthorization {
        current
    }

    public func request() async -> NotificationAuthorization {
        requestCount += 1
        // The system asks once: a later request returns the earlier choice.
        if current == .notDetermined { current = answer }
        return current
    }
}
