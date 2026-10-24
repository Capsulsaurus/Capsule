import CapsuleDomain
import Foundation

// MARK: - AlertPolicyPort

/// The alert predicate — `capsule-core::notify`, one decision function for
/// every platform.
///
/// A port rather than a Swift reimplementation: the real adapter is a thin
/// call into the core's `evaluate_alerts` / `pre_arm_deadlines` exports, and
/// only the mock lane carries an in-memory stand-in. Both are pure, so the
/// methods are synchronous.
public protocol AlertPolicyPort: Sendable {
    /// Every alert that is true at `now`, in delivery order. The in-app badge
    /// set: every class reported here is shown in-app whether or not a
    /// notification was ever armed for it.
    func evaluate(_ input: NotifyInput, at now: CapsuleTimestamp) throws -> [Alert]

    /// The instant to arm a notification for, **per class**.
    ///
    /// A class present holds exactly one alarm at that instant; a class absent
    /// holds none. This is the per-class call — never collapse it to the
    /// earliest deadline, which loses the later class's alarm on a device the
    /// app is not opened on again.
    func preArmDeadlines(_ input: NotifyInput, at now: CapsuleTimestamp) throws -> [ClassDeadline]
}

// MARK: - AlertFactsPort

/// The device-held state the predicate reads: the last completed sync, the
/// un-synced count, the last quota state, the quarantine and drop counts, the
/// recovery cadence, and the user's snoozes and disables.
public protocol AlertFactsPort: Sendable {
    /// The current facts. A fact that cannot be read right now is absent from
    /// the result rather than an error, so an offline quota read never
    /// silences the sync alert.
    func facts() async -> NotifyInput
}

// MARK: - AlertSchedulerPort

/// One notification armed with the OS for a class.
public struct ArmedAlarm: Sendable, Hashable {
    /// What it presents when it fires — the class and the parameters its copy
    /// interpolates, fixed when it was armed.
    public var alert: Alert
    /// When it fires.
    public var fireAt: CapsuleTimestamp

    public init(alert: Alert, fireAt: CapsuleTimestamp) {
        self.alert = alert
        self.fireAt = fireAt
    }

    public var alertClass: AlertClass {
        alert.alertClass
    }
}

/// The OS's local-notification scheduler: `UNUserNotificationCenter` on Apple
/// platforms.
///
/// Keyed by class. An implementation holds **at most one** alarm per class —
/// arming a class that already holds one replaces it — so two live timers for
/// one class cannot exist whatever the caller does.
public protocol AlertSchedulerPort: Sendable {
    /// The alarms currently armed, by class.
    func armed() async -> [AlertClass: ArmedAlarm]

    /// Arm `alarm`, replacing any alarm its class already holds.
    func arm(_ alarm: ArmedAlarm) async throws

    /// Cancel whatever alarm `alertClass` holds. A notification that already
    /// fired is left where the user can see it.
    func cancel(_ alertClass: AlertClass) async
}

// MARK: - NotificationAuthorizationPort

/// Whether the user lets the app post notifications.
public enum NotificationAuthorization: Sendable, Hashable {
    /// Never asked.
    case notDetermined
    /// Allowed.
    case authorized
    /// Refused, now or earlier. Not an error: every class degrades to its
    /// in-app badge.
    case denied
}

/// The notification-permission prompt.
///
/// The prompt is requested the first time a class has something to arm — so
/// it arrives with context — and never at launch.
public protocol NotificationAuthorizationPort: Sendable {
    /// The current authorization, without prompting.
    func status() async -> NotificationAuthorization

    /// Ask the user, once. A failure to ask is reported as ``NotificationAuthorization/denied``.
    func request() async -> NotificationAuthorization
}
