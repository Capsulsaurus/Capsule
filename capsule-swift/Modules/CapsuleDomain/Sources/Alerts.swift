import Foundation

// MARK: - AlertClass

/// One of the local alert classes — the closed set `capsule-core::notify`
/// decides (*Notifications — Alert Classes*).
///
/// The raw value is the core's wire name. It is the same string the
/// `suppressed_until` and `disabled` inputs are keyed by and the segment that
/// selects a `notification.*` catalog key, so it is a contract rather than a
/// spelling. Unlike a ``ClosedWireEnum`` this set never arrives from a peer —
/// the device decides it locally — so it has no `unknown` case to preserve.
public enum AlertClass: String, Sendable, Hashable, CaseIterable, Codable {
    /// Two weeks without a completed sync while changes remain un-synced.
    case syncStale = "sync_stale"
    /// A recovery-secret verification check is due.
    case recoveryCheckDue = "recovery_check_due"
    /// Storage crossed the soft limit and is below the hard limit.
    case quotaSoft = "quota_soft"
    /// Storage is over the hard limit — the grace window is counting, or has
    /// closed.
    case quotaGraceExpiring = "quota_grace_expiring"
    /// Items sit on a quarantine surface awaiting a human.
    case quarantinePending = "quarantine_pending"
    /// Guest drops await review and adoption.
    case dropPending = "drop_pending"

    /// Whether the device can compute this class's deadline alone, and so
    /// pre-arm an OS timer for it. The other four are server-held conditions
    /// that surface at next launch.
    public var isPreArmable: Bool {
        switch self {
        case .syncStale, .recoveryCheckDue: true
        case .quotaSoft, .quotaGraceExpiring, .quarantinePending, .dropPending: false
        }
    }

    /// The class's fixed severity, as the core assigns it.
    public var severity: AlertSeverity {
        switch self {
        case .syncStale, .quotaGraceExpiring, .quarantinePending: .warning
        case .recoveryCheckDue, .quotaSoft, .dropPending: .advisory
        }
    }
}

// MARK: - AlertSeverity

/// How prominently an alert is presented. Neither value gates any flow.
public enum AlertSeverity: Sendable, Hashable {
    /// Nothing is failing, and nothing is about to.
    case advisory
    /// Something is already degraded or is being refused.
    case warning
}

// MARK: - Alert

/// One alert that is true at an instant: a class and the plain values its
/// catalog copy interpolates. Never text — the words come from the app's own
/// `notification.*` catalog, keyed by ``copy``.
public struct Alert: Sendable, Hashable {
    /// The parameter names the predicate emits (*Notifications — Alert
    /// Classes*). Spelled once here so a typo is a compile error.
    public enum Parameter {
        public static let count = "count"
        public static let daysBehind = "days_behind"
        public static let grace = "grace"
        public static let snoozeBudget = "snooze_budget"
        public static let recovery = "recovery"
    }

    public var alertClass: AlertClass
    /// The instant whose passing made the alert true, for a pre-armable class.
    public var deadline: CapsuleTimestamp?
    /// The predicate's parameters, by ``Parameter`` name.
    public var params: [String: String]

    public init(
        alertClass: AlertClass,
        deadline: CapsuleTimestamp? = nil,
        params: [String: String] = [:]
    ) {
        self.alertClass = alertClass
        self.deadline = deadline
        self.params = params
    }

    /// How prominently to present it — fixed per class.
    public var severity: AlertSeverity {
        alertClass.severity
    }

    /// Whether this recovery alert is the guided re-wrap rather than the
    /// routine check. The class set is closed, so this parameter is the only
    /// thing that tells the two apart — and it changes where a tap goes, not
    /// only the words.
    public var isGuidedRewrap: Bool {
        alertClass == .recoveryCheckDue && params[Parameter.recovery] == "rewrap"
    }

    /// Whether the class's snooze budget is spent: it has degraded to a
    /// persistent badge and is never armed as a notification again.
    public var isSnoozeBudgetSpent: Bool {
        params[Parameter.snoozeBudget] == "spent"
    }

    /// The catalog copy this alert renders.
    public var copy: AlertCopy {
        AlertCopy(alert: self)
    }

    /// Where opening this alert leads.
    public var destination: AlertDestination {
        switch alertClass {
        case .syncStale: .syncStatus
        case .recoveryCheckDue: isGuidedRewrap ? .guidedRewrap : .recoveryVerification
        case .quotaSoft, .quotaGraceExpiring: .quota
        case .quarantinePending: .quarantine
        case .dropPending: .drops
        }
    }
}

// MARK: - AlertDestination

/// Where opening an alert leads, before the app turns it into a route.
///
/// Kept apart from `Route` because the domain cannot name navigation, and so
/// the re-wrap / check split is asserted here, once, rather than in every
/// shell that handles a notification tap.
public enum AlertDestination: String, Sendable, Hashable, CaseIterable {
    case syncStatus = "sync_status"
    case recoveryVerification = "recovery_verification"
    /// The guided re-wrap — never the routine verification prompt.
    case guidedRewrap = "guided_rewrap"
    case quota
    case quarantine
    case drops
}

// MARK: - AlertCopy

/// The catalog keys and arguments one alert renders with.
///
/// The `notification.*` messages use a whole-message plural at most (the only
/// ICU form the generator compiles), so an enumerated parameter such as
/// `recovery` or `grace` selects a *key* rather than an ICU `select` arm, and
/// a count is a plural body's only argument. `sync_stale`'s second number,
/// `days_behind`, is therefore its *title*'s text argument.
public struct AlertCopy: Sendable, Hashable {
    /// The `notification.*` key of the title.
    public let titleKey: String
    /// The title's text arguments, in specifier order.
    public let titleArguments: [String]
    /// The `notification.*` key of the body.
    public let bodyKey: String
    /// The body's plural count, when the body is a plural — its only argument.
    public let count: Int?

    public init(alert: Alert) {
        let stem: String
        var count: Int?
        var title: [String] = []
        let params = alert.params
        switch alert.alertClass {
        case .syncStale:
            stem = "notification.sync_stale"
            count = Self.integer(params[Alert.Parameter.count])
            title = [params[Alert.Parameter.daysBehind] ?? String(SyncStatus.stalenessThresholdDays)]
        case .recoveryCheckDue:
            stem = alert.isGuidedRewrap
                ? "notification.recovery_check_due.rewrap"
                : "notification.recovery_check_due.check"
        case .quotaSoft:
            stem = "notification.quota_soft"
        case .quotaGraceExpiring:
            stem = params[Alert.Parameter.grace] == "expired"
                ? "notification.quota_grace_expiring.expired"
                : "notification.quota_grace_expiring.counting"
        case .quarantinePending:
            stem = "notification.quarantine_pending"
            count = Self.integer(params[Alert.Parameter.count])
        case .dropPending:
            stem = "notification.drop_pending"
            count = Self.integer(params[Alert.Parameter.count])
        }
        titleKey = "\(stem).title"
        titleArguments = title
        bodyKey = "\(stem).body"
        self.count = count
    }

    /// A count parameter as an integer. A malformed one renders as zero
    /// rather than dropping the alert: the class is still true.
    private static func integer(_ raw: String?) -> Int {
        raw.flatMap { Int($0) } ?? 0
    }
}

// MARK: - ClassDeadline

/// One pre-armable class and the instant its notification is armed for.
public struct ClassDeadline: Sendable, Hashable {
    public var alertClass: AlertClass
    public var deadline: CapsuleTimestamp

    public init(alertClass: AlertClass, deadline: CapsuleTimestamp) {
        self.alertClass = alertClass
        self.deadline = deadline
    }
}

// MARK: - NotifyInput

/// The device-held state the alert predicate decides from — the Swift mirror
/// of the core's flat `FfiNotifyInput`.
///
/// An all-default value is "just installed, learned nothing", and yields no
/// alert and no deadline.
public struct NotifyInput: Sendable, Hashable {
    /// When the last **completed** sync finished. `nil` ignores
    /// ``unsyncedChanges``.
    public var lastCompletedSync: CapsuleTimestamp?
    /// Changes still waiting to reach the server.
    public var unsyncedChanges: Int
    /// When the next recovery verification is due. `nil` before recovery is
    /// set up, which ignores the other `recovery*` fields.
    public var recoveryNextDue: CapsuleTimestamp?
    /// When an active snooze on the recovery prompt ends.
    public var recoverySnoozedUntil: CapsuleTimestamp?
    /// Whether the recovery snooze budget is spent (a persistent badge).
    public var recoverySnoozeBudgetSpent: Bool
    /// Whether the cadence escalated to the guided re-wrap.
    public var recoveryRewrapDue: Bool
    /// The quota state from the last quota read. `nil` before the first.
    public var quotaState: QuotaState?
    /// Items on the quarantine surfaces, **excluding** pending drops.
    public var quarantinePending: Int
    /// Guest drops awaiting review.
    public var dropsPending: Int
    /// Per-class snooze, exclusive. Defers an armed notification to its end.
    public var suppressedUntil: [AlertClass: CapsuleTimestamp]
    /// Classes the user turned off. They report nothing and hold no alarm.
    public var disabled: Set<AlertClass>

    public init(
        lastCompletedSync: CapsuleTimestamp? = nil,
        unsyncedChanges: Int = 0,
        recoveryNextDue: CapsuleTimestamp? = nil,
        recoverySnoozedUntil: CapsuleTimestamp? = nil,
        recoverySnoozeBudgetSpent: Bool = false,
        recoveryRewrapDue: Bool = false,
        quotaState: QuotaState? = nil,
        quarantinePending: Int = 0,
        dropsPending: Int = 0,
        suppressedUntil: [AlertClass: CapsuleTimestamp] = [:],
        disabled: Set<AlertClass> = []
    ) {
        self.lastCompletedSync = lastCompletedSync
        self.unsyncedChanges = unsyncedChanges
        self.recoveryNextDue = recoveryNextDue
        self.recoverySnoozedUntil = recoverySnoozedUntil
        self.recoverySnoozeBudgetSpent = recoverySnoozeBudgetSpent
        self.recoveryRewrapDue = recoveryRewrapDue
        self.quotaState = quotaState
        self.quarantinePending = quarantinePending
        self.dropsPending = dropsPending
        self.suppressedUntil = suppressedUntil
        self.disabled = disabled
    }

    /// The alert an armed notification for `deadline` will present when it
    /// fires.
    ///
    /// The core returns only the class and the instant, and an armed
    /// notification fires with the app not running, so its copy is fixed
    /// now, from the state that produced the deadline: `sync_stale` carries
    /// the current un-synced count and the whole days between the last sync
    /// and the deadline; `recovery_check_due` carries the check / re-wrap
    /// split. A pre-armed recovery alert always has budget left — the core
    /// withholds the deadline once it is spent.
    public func armedAlert(for deadline: ClassDeadline) -> Alert {
        var params: [String: String] = [:]
        switch deadline.alertClass {
        case .syncStale:
            params[Alert.Parameter.count] = String(unsyncedChanges)
            let since = lastCompletedSync?.epochSeconds ?? deadline.deadline.epochSeconds
            let days = max(0, deadline.deadline.epochSeconds - since) / 86400
            params[Alert.Parameter.daysBehind] = String(days)
        case .recoveryCheckDue:
            params[Alert.Parameter.snoozeBudget] = "available"
            params[Alert.Parameter.recovery] = recoveryRewrapDue ? "rewrap" : "check"
        case .quotaSoft, .quotaGraceExpiring, .quarantinePending, .dropPending:
            break
        }
        return Alert(
            alertClass: deadline.alertClass,
            deadline: deadline.deadline,
            params: params
        )
    }
}
