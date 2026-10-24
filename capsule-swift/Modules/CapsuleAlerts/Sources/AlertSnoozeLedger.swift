import CapsuleDomain
import Foundation

// MARK: - AlertSnoozeOutcome

/// What a snooze request did.
public enum AlertSnoozeOutcome: Sendable, Hashable {
    /// Suppressed until the instant; an armed notification is deferred to it.
    case snoozed(until: CapsuleTimestamp)
    /// The budget is spent: the class is a persistent in-app badge from now
    /// on, and never escalates back to a notification until it clears.
    case badgeOnly
    /// The recovery check is snoozed by its own cadence scheduler, which owns
    /// the budget that bounds it — not by this ledger.
    case ownedByRecoveryCadence
}

// MARK: - AlertSnoozeLedger

/// The client's bounded-snooze-then-badge state for the five classes the
/// recovery cadence does not own.
///
/// *Notifications — Shared Alert Mechanics*: a snooze is bounded, then the
/// class becomes a badge that never escalates back to an alert. The core
/// takes only the suppression instant (`suppressed_until`), so the count that
/// bounds it lives here, once. A class's count resets when its condition
/// clears — the next time it comes true it is a new alert, with a fresh
/// budget.
///
/// A value type: the coordinator owns one, persists it through an
/// ``AlertSnoozeStore``, and every rule is a pure function of it.
public struct AlertSnoozeLedger: Sendable, Hashable, Codable {
    /// How long one snooze lasts.
    public static let interval: Int64 = 86400
    /// Consecutive snoozes before the class is badge-only — the same bound
    /// the recovery cadence applies (``RecoveryVerificationState/maximumConsecutiveSnoozes``).
    public static let maximumConsecutiveSnoozes = RecoveryVerificationState.maximumConsecutiveSnoozes

    /// One class's snooze state.
    public struct Entry: Sendable, Hashable, Codable {
        /// The snooze end, exclusive, in epoch seconds.
        public var untilEpochSeconds: Int64
        /// Consecutive snoozes taken since the condition last cleared.
        public var count: Int
    }

    /// By class wire name, so the persisted form survives a class rename on
    /// neither side.
    public private(set) var entries: [String: Entry]

    public init(entries: [String: Entry] = [:]) {
        self.entries = entries
    }

    /// Snooze `alertClass` from `now`.
    public mutating func snooze(_ alertClass: AlertClass, now: CapsuleTimestamp) -> AlertSnoozeOutcome {
        if alertClass == .recoveryCheckDue { return .ownedByRecoveryCadence }
        let count = entries[alertClass.rawValue]?.count ?? 0
        guard count < Self.maximumConsecutiveSnoozes else { return .badgeOnly }
        let until = now.epochSeconds + Self.interval
        entries[alertClass.rawValue] = Entry(untilEpochSeconds: until, count: count + 1)
        return .snoozed(until: CapsuleTimestamp(epochSeconds: until))
    }

    /// The suppression instants to hand the predicate. Includes an expired
    /// one: it is exclusive, so the predicate ignores it, and dropping it here
    /// would add nothing.
    public var suppressions: [AlertClass: CapsuleTimestamp] {
        var result: [AlertClass: CapsuleTimestamp] = [:]
        for (name, entry) in entries {
            guard let alertClass = AlertClass(rawValue: name) else { continue }
            result[alertClass] = CapsuleTimestamp(epochSeconds: entry.untilEpochSeconds)
        }
        return result
    }

    /// The classes whose budget is spent: shown in-app, never armed.
    public var badgeOnly: Set<AlertClass> {
        Set(entries.compactMap { name, entry in
            entry.count >= Self.maximumConsecutiveSnoozes ? AlertClass(rawValue: name) : nil
        })
    }

    /// Forget every class that is no longer true. Returns whether anything
    /// changed, so the caller persists only when it must.
    @discardableResult
    public mutating func clear(keeping stillTrue: Set<AlertClass>) -> Bool {
        let before = entries.count
        entries = entries.filter { name, _ in
            AlertClass(rawValue: name).map(stillTrue.contains) ?? false
        }
        return entries.count != before
    }
}

// MARK: - AlertSnoozeStore

/// Where the ledger persists between launches.
public protocol AlertSnoozeStore: Sendable {
    func load() -> AlertSnoozeLedger
    func save(_ ledger: AlertSnoozeLedger)
}

/// The ledger in `UserDefaults`, JSON-encoded under one key.
///
/// A snooze is a preference about a reminder rather than library data, and
/// losing it only means a reminder arrives a day early — so a defaults key is
/// proportionate, and a decode failure starts from an empty ledger.
public struct UserDefaultsAlertSnoozeStore: AlertSnoozeStore, @unchecked Sendable {
    // `UserDefaults` is documented thread-safe; it is not annotated `Sendable`.
    private let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = "capsule.alerts.snoozes") {
        self.defaults = defaults
        self.key = key
    }

    public func load() -> AlertSnoozeLedger {
        guard let data = defaults.data(forKey: key),
              let ledger = try? JSONDecoder().decode(AlertSnoozeLedger.self, from: data)
        else { return AlertSnoozeLedger() }
        return ledger
    }

    public func save(_ ledger: AlertSnoozeLedger) {
        guard let data = try? JSONEncoder().encode(ledger) else { return }
        defaults.set(data, forKey: key)
    }
}

/// A ledger held only in memory — for tests and previews.
public final class InMemoryAlertSnoozeStore: AlertSnoozeStore, @unchecked Sendable {
    private let lock = NSLock()
    private var ledger: AlertSnoozeLedger

    public init(_ ledger: AlertSnoozeLedger = AlertSnoozeLedger()) {
        self.ledger = ledger
    }

    public func load() -> AlertSnoozeLedger {
        lock.withLock { ledger }
    }

    public func save(_ ledger: AlertSnoozeLedger) {
        lock.withLock { self.ledger = ledger }
    }
}
