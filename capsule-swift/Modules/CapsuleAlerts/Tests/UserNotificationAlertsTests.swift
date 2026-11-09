import CapsulePorts
import Testing
import UserNotifications

@testable import CapsuleAlerts

@Suite("The system authorization maps onto the port's three states")
struct UserNotificationAlertsTests {
    @Test("provisional delivery counts as allowed; anything refused or unknown degrades to badges")
    func authorizationMapping() {
        #expect(UserNotificationAuthorization.map(.notDetermined) == .notDetermined)
        #expect(UserNotificationAuthorization.map(.authorized) == .authorized)
        #expect(UserNotificationAuthorization.map(.provisional) == .authorized)
        #expect(UserNotificationAuthorization.map(.denied) == .denied)
    }
}
