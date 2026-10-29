import CapsuleDomain
import CapsulePorts

// MARK: - AlertTimerOperation

/// One change to the OS's armed notifications.
public enum AlertTimerOperation: Sendable, Hashable {
    /// Cancel whatever the class holds.
    case cancel(AlertClass)
    /// Arm this alarm, replacing whatever its class holds.
    case arm(ArmedAlarm)
}

// MARK: - AlertTimerPlan

/// The arm / re-arm / cancel rule, as a pure function.
///
/// The whole client-side protocol is "recompute the per-class deadlines after
/// any state change, then reconcile the armed timers against them". This is
/// the reconcile half, kept free of the OS so the lifecycle can be asserted
/// under a mocked clock:
///
/// - a class **wanted but not armed** is armed;
/// - a class **armed but not wanted** is cancelled — its condition cleared,
///   it was disabled, or its snooze budget is spent;
/// - a class **armed differently** (another instant, or other copy) is
///   cancelled and then armed again, so its one timer moves rather than a
///   second one appearing;
/// - a class armed exactly as wanted is left alone, so a refresh that changed
///   nothing touches nothing.
///
/// Operations come out in ``AlertClass`` declaration order, cancels before
/// arms within a class, so the plan is deterministic.
public enum AlertTimerPlan {
    public static func reconcile(
        desired: [ArmedAlarm],
        armed: [AlertClass: ArmedAlarm]
    ) -> [AlertTimerOperation] {
        // Last entry per class wins, so a duplicated input can never produce
        // two arms for one class.
        let wanted = Dictionary(desired.map { ($0.alertClass, $0) }, uniquingKeysWith: { _, last in last })
        var operations: [AlertTimerOperation] = []
        for alertClass in AlertClass.allCases {
            switch (armed[alertClass], wanted[alertClass]) {
            case (nil, nil):
                continue
            case (.some, nil):
                operations.append(.cancel(alertClass))
            case let (nil, .some(alarm)):
                operations.append(.arm(alarm))
            case let (.some(current), .some(alarm)):
                guard current != alarm else { continue }
                operations.append(.cancel(alertClass))
                operations.append(.arm(alarm))
            }
        }
        return operations
    }
}
