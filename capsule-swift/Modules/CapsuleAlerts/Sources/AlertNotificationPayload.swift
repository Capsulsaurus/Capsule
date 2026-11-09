import CapsuleDomain
import CapsulePorts
import Foundation

// MARK: - AlertNotificationPayload

/// How an armed alarm is written into, and read back out of, an OS
/// notification request.
///
/// The request identifier is one per **class** (`capsule.alert.<wire name>`),
/// which is what makes the OS itself enforce "never two live timers per
/// class": adding a request under an identifier that is already pending
/// replaces it. The `userInfo` carries the class, the fire instant and the
/// copy parameters, so ``AlertSchedulerPort/armed()`` can return exactly what
/// was armed and a tap can be routed — re-wrap or check — without re-running
/// the predicate. Values are strings throughout, so nothing depends on how a
/// platform bridges a number.
public enum AlertNotificationPayload {
    static let identifierPrefix = "capsule.alert."
    static let classKey = "capsule.alert.class"
    static let fireAtKey = "capsule.alert.fire_at"
    static let paramsKey = "capsule.alert.params"

    /// The request identifier for a class.
    public static func identifier(for alertClass: AlertClass) -> String {
        identifierPrefix + alertClass.rawValue
    }

    /// The class a request identifier belongs to, or `nil` for a request this
    /// module did not make.
    public static func alertClass(forIdentifier identifier: String) -> AlertClass? {
        guard identifier.hasPrefix(identifierPrefix) else { return nil }
        return AlertClass(rawValue: String(identifier.dropFirst(identifierPrefix.count)))
    }

    /// The `userInfo` an armed alarm's request carries.
    public static func userInfo(for alarm: ArmedAlarm) -> [String: Any] {
        [
            classKey: alarm.alertClass.rawValue,
            fireAtKey: String(alarm.fireAt.epochSeconds),
            paramsKey: alarm.alert.params,
        ]
    }

    /// The alarm a request's `userInfo` describes, or `nil` when it is not one
    /// of ours or is malformed.
    public static func alarm(from userInfo: [AnyHashable: Any]) -> ArmedAlarm? {
        guard let name = userInfo[classKey] as? String,
              let alertClass = AlertClass(rawValue: name),
              let fireAtText = userInfo[fireAtKey] as? String,
              let fireAt = Int64(fireAtText)
        else { return nil }
        let params = userInfo[paramsKey] as? [String: String] ?? [:]
        let instant = CapsuleTimestamp(epochSeconds: fireAt)
        return ArmedAlarm(alert: Alert(alertClass: alertClass, deadline: instant, params: params), fireAt: instant)
    }
}
