import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import UserNotifications

// MARK: - UserNotificationAlertScheduler

/// ``AlertSchedulerPort`` over `UNUserNotificationCenter` — the native API
/// directly, with no abstraction between (*Notifications — Delivery*).
///
/// Each alarm is a `UNCalendarNotificationTrigger` at an absolute UTC instant,
/// so it fires with the app not running and is unaffected by a time-zone
/// change between arming and firing. One request identifier per class
/// (``AlertNotificationPayload``) means the OS replaces rather than adds.
public struct UserNotificationAlertScheduler: AlertSchedulerPort {
    private let bundle: Bundle

    /// - Parameter bundle: where the `notification.*` catalog resolves — the
    ///   app bundle, which ships the generated `Localizable.xcstrings`.
    public init(bundle: Bundle = .main) {
        self.bundle = bundle
    }

    public func armed() async -> [AlertClass: ArmedAlarm] {
        let requests = await UNUserNotificationCenter.current().pendingNotificationRequests()
        var result: [AlertClass: ArmedAlarm] = [:]
        for request in requests {
            guard let alertClass = AlertNotificationPayload.alertClass(forIdentifier: request.identifier) else {
                continue
            }
            // A request this build cannot read back is still ours: report it
            // with nothing in it, so the reconcile replaces it.
            result[alertClass] = AlertNotificationPayload.alarm(from: request.content.userInfo)
                ?? ArmedAlarm(alert: Alert(alertClass: alertClass), fireAt: CapsuleTimestamp(epochSeconds: 0))
        }
        return result
    }

    public func arm(_ alarm: ArmedAlarm) async throws {
        let text = AlertNotificationText(copy: alarm.alert.copy, bundle: bundle)
        let content = UNMutableNotificationContent()
        content.title = text.title
        content.body = text.body
        content.sound = .default
        content.threadIdentifier = AlertNotificationPayload.identifier(for: alarm.alertClass)
        content.userInfo = AlertNotificationPayload.userInfo(for: alarm)

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        let components = calendar.dateComponents(
            [.calendar, .timeZone, .year, .month, .day, .hour, .minute, .second],
            from: alarm.fireAt.date
        )
        let request = UNNotificationRequest(
            identifier: AlertNotificationPayload.identifier(for: alarm.alertClass),
            content: content,
            trigger: UNCalendarNotificationTrigger(dateMatching: components, repeats: false)
        )
        try await UNUserNotificationCenter.current().add(request)
    }

    public func cancel(_ alertClass: AlertClass) async {
        // Pending only: a notification that already fired stays where the
        // user can see it until they act on it or the condition is re-armed.
        UNUserNotificationCenter.current().removePendingNotificationRequests(
            withIdentifiers: [AlertNotificationPayload.identifier(for: alertClass)]
        )
    }
}

// MARK: - AlertNotificationText

/// An alert's catalog copy, resolved to the words a notification shows.
///
/// Resolved when the alarm is **armed**, because it fires with the app not
/// running and the OS shows whatever text it was given.
struct AlertNotificationText {
    let title: String
    let body: String

    init(copy: AlertCopy, bundle: Bundle) {
        title = bundle.localizedString(forKey: copy.titleKey, value: nil, table: nil)
        let format = bundle.localizedString(forKey: copy.bodyKey, value: nil, table: nil)
        var arguments: [any CVarArg] = []
        if let count = copy.count { arguments.append(count) }
        arguments.append(contentsOf: copy.textArguments.map { $0 as NSString })
        body = arguments.isEmpty ? format : String(format: format, locale: .current, arguments: arguments)
    }
}

// MARK: - UserNotificationAuthorization

/// ``NotificationAuthorizationPort`` over `UNUserNotificationCenter`'s
/// authorization prompt.
public struct UserNotificationAuthorization: NotificationAuthorizationPort {
    public init() {}

    public func status() async -> NotificationAuthorization {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        return Self.map(settings.authorizationStatus)
    }

    public func request() async -> NotificationAuthorization {
        do {
            let granted = try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound, .badge])
            return granted ? .authorized : .denied
        } catch {
            // Not an error to the user: a prompt that could not be shown is a
            // refusal, and every class falls back to its in-app badge.
            CapsuleLog.alerts.error("notification authorization request failed: \(String(describing: error), privacy: .public)")
            return .denied
        }
    }

    static func map(_ status: UNAuthorizationStatus) -> NotificationAuthorization {
        switch status {
        case .notDetermined: .notDetermined
        case .authorized, .provisional: .authorized
        default: .denied
        }
    }
}

// MARK: - AlertNotificationResponder

/// The notification-center delegate: shows an alert that fires while the app
/// is in the foreground, and turns a tap into an ``AlertDestination``.
///
/// The destination is decided from the notification's own payload, so a
/// re-wrap alert opens the re-wrap and never the routine check, even when the
/// app was not running when it fired. Hold one for the app's lifetime and
/// install it before launch finishes, or a tap that launched the app is lost.
public final class AlertNotificationResponder: NSObject, UNUserNotificationCenterDelegate, Sendable {
    /// Every tapped alert's destination, in order.
    public let destinations: AsyncStream<AlertDestination>
    private let continuation: AsyncStream<AlertDestination>.Continuation

    override public init() {
        let pair = AsyncStream.makeStream(of: AlertDestination.self, bufferingPolicy: .bufferingNewest(1))
        destinations = pair.stream
        continuation = pair.continuation
        super.init()
    }

    /// Become the notification center's delegate.
    @MainActor
    public func install() {
        UNUserNotificationCenter.current().delegate = self
    }

    public func userNotificationCenter(
        _: UNUserNotificationCenter,
        willPresent _: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    public func userNotificationCenter(
        _: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        guard let alarm = AlertNotificationPayload.alarm(from: response.notification.request.content.userInfo) else {
            return
        }
        let destination = alarm.alert.destination
        CapsuleLog.alerts.info(
            "alert tapped: \(alarm.alertClass.rawValue, privacy: .public) -> \(destination.rawValue, privacy: .public)"
        )
        continuation.yield(destination)
    }
}
