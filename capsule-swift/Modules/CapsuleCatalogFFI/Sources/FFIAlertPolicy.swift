import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - FFIAlertPolicy

/// ``AlertPolicyPort`` over the core's exports — `evaluate_alerts` and
/// `pre_arm_deadlines` from `capsule-sdk::ffi::notify`.
///
/// Nothing here decides anything. It converts the facts to the flat FFI record
/// (instants as RFC 3339, counts as unsigned) and the answer back, so the
/// predicate the app runs is the one `capsule-core::notify` defines and the
/// mock lane's `MockAlertPolicy` only imitates.
///
/// A refused input is rethrown as ``FFIAlertPolicyError``; the coordinator
/// treats any throw as "leave the armed alarms as they were", so a bad fact
/// degrades one refresh, never the armed timers.
public struct FFIAlertPolicy: AlertPolicyPort {
    public init() {}

    public func evaluate(_ input: NotifyInput, at now: CapsuleTimestamp) throws -> [Alert] {
        let alerts = try call { try evaluateAlerts(input: Self.ffi(input), now: now.rfc3339) }
        return try alerts.map { alert in
            try Alert(
                alertClass: Self.alertClass(alert.`class`),
                deadline: alert.deadline.map { try Self.instant($0, field: "deadline") },
                params: alert.params
            )
        }
    }

    public func preArmDeadlines(_ input: NotifyInput, at now: CapsuleTimestamp) throws -> [ClassDeadline] {
        let deadlines = try call { try CapsuleCatalogFFI.preArmDeadlines(input: Self.ffi(input), now: now.rfc3339) }
        return try deadlines.map {
            try ClassDeadline(alertClass: Self.alertClass($0.`class`), deadline: Self.instant($0.deadline, field: "deadline"))
        }
    }

    // MARK: Conversions

    /// The flat FFI record. A negative count — which a Swift `Int` can hold
    /// and the core's `u64` cannot — is clamped to zero rather than trapping.
    static func ffi(_ input: NotifyInput) -> FfiNotifyInput {
        FfiNotifyInput(
            lastCompletedSync: input.lastCompletedSync?.rfc3339,
            unsyncedChanges: UInt64(max(0, input.unsyncedChanges)),
            recoveryNextDue: input.recoveryNextDue?.rfc3339,
            recoverySnoozedUntil: input.recoverySnoozedUntil?.rfc3339,
            recoverySnoozeBudgetSpent: input.recoverySnoozeBudgetSpent,
            recoveryRewrapDue: input.recoveryRewrapDue,
            quotaState: input.quotaState.flatMap(quotaAdvisory),
            quarantinePending: UInt64(max(0, input.quarantinePending)),
            dropsPending: UInt64(max(0, input.dropsPending)),
            suppressedUntil: Dictionary(
                uniqueKeysWithValues: input.suppressedUntil.map { ($0.key.rawValue, $0.value.rfc3339) }
            ),
            disabled: input.disabled.map(\.rawValue).sorted()
        )
    }

    /// The core's quota advisory. A state this build does not know raises no
    /// quota alert, exactly as an unread quota does.
    static func quotaAdvisory(_ state: QuotaState) -> FfiQuotaAdvisory? {
        switch state {
        case .withinQuota: .ok
        case .softWarning: .softWarning
        case .hardExceeded: .hardExceeded
        case .graceExpired: .graceExpired
        case .suspended: .suspended
        case .unknown: nil
        }
    }

    static func alertClass(_ ffi: FfiAlertClass) -> AlertClass {
        switch ffi {
        case .syncStale: .syncStale
        case .recoveryCheckDue: .recoveryCheckDue
        case .quotaSoft: .quotaSoft
        case .quotaGraceExpiring: .quotaGraceExpiring
        case .quarantinePending: .quarantinePending
        case .dropPending: .dropPending
        }
    }

    /// An instant the core rendered. One that does not parse is a broken
    /// boundary, reported rather than read as the epoch.
    static func instant(_ raw: String, field: String) throws -> CapsuleTimestamp {
        guard let timestamp = CapsuleTimestamp(rfc3339: raw) else {
            throw FFIAlertPolicyError.malformedOutput(field: field, value: raw)
        }
        return timestamp
    }

    private func call<T>(_ body: () throws -> T) throws -> T {
        do {
            return try body()
        } catch let error as FfiError {
            CapsuleLog.alerts.error("alert predicate refused its input: \(String(describing: error), privacy: .public)")
            throw FFIAlertPolicyError.refused(String(describing: error))
        }
    }
}

// MARK: - FFIAlertPolicyError

/// Why the core's alert predicate gave no answer.
public enum FFIAlertPolicyError: Error, Equatable {
    /// The core refused the input — a malformed instant or class name.
    case refused(String)
    /// The core answered with an instant that does not parse.
    case malformedOutput(field: String, value: String)
}
