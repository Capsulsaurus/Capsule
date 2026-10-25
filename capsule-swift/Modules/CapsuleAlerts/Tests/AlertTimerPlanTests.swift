import CapsuleDomain
import CapsulePorts
import Testing

@testable import CapsuleAlerts

@Suite("The timer plan reconciles armed alarms against the per-class deadlines")
struct AlertTimerPlanTests {
    private func alarm(_ alertClass: AlertClass, at instant: CapsuleTimestamp, count: String? = nil) -> ArmedAlarm {
        let params = count.map { ["count": $0] } ?? [:]
        return ArmedAlarm(alert: Alert(alertClass: alertClass, deadline: instant, params: params), fireAt: instant)
    }

    @Test("a wanted class that holds nothing is armed")
    func arms() {
        let wanted = alarm(.syncStale, at: day(14))
        #expect(AlertTimerPlan.reconcile(desired: [wanted], armed: [:]) == [.arm(wanted)])
    }

    @Test("an armed class that is no longer wanted is cancelled")
    func cancels() {
        let armed = alarm(.recoveryCheckDue, at: day(90))
        #expect(AlertTimerPlan.reconcile(desired: [], armed: [.recoveryCheckDue: armed]) == [.cancel(.recoveryCheckDue)])
    }

    @Test("a moved deadline cancels then re-arms the one timer")
    func reArmsMovedDeadline() {
        let old = alarm(.syncStale, at: day(14))
        let new = alarm(.syncStale, at: day(20))
        #expect(AlertTimerPlan.reconcile(desired: [new], armed: [.syncStale: old]) == [.cancel(.syncStale), .arm(new)])
    }

    @Test("changed copy at the same instant re-arms too, so the notification is not stale")
    func reArmsChangedCopy() {
        let old = alarm(.syncStale, at: day(14), count: "2")
        let new = alarm(.syncStale, at: day(14), count: "5")
        #expect(AlertTimerPlan.reconcile(desired: [new], armed: [.syncStale: old]) == [.cancel(.syncStale), .arm(new)])
    }

    @Test("an alarm armed exactly as wanted is left alone")
    func leavesEqual() {
        let same = alarm(.syncStale, at: day(14))
        #expect(AlertTimerPlan.reconcile(desired: [same], armed: [.syncStale: same]).isEmpty)
    }

    @Test("a class listed twice still gets one arm — the last")
    func duplicatesCollapse() {
        let first = alarm(.syncStale, at: day(14))
        let second = alarm(.syncStale, at: day(15))
        #expect(AlertTimerPlan.reconcile(desired: [first, second], armed: [:]) == [.arm(second)])
    }

    @Test("operations come out in class order")
    func deterministicOrder() {
        let sync = alarm(.syncStale, at: day(14))
        let recovery = alarm(.recoveryCheckDue, at: day(90))
        #expect(AlertTimerPlan.reconcile(desired: [recovery, sync], armed: [:]) == [.arm(sync), .arm(recovery)])
    }
}

@Suite("Armed alarms round-trip through the notification payload")
struct AlertNotificationPayloadTests {
    @Test("one identifier per class, and only ours are recognised")
    func identifiers() {
        for alertClass in AlertClass.allCases {
            let identifier = AlertNotificationPayload.identifier(for: alertClass)
            #expect(AlertNotificationPayload.alertClass(forIdentifier: identifier) == alertClass)
        }
        #expect(AlertNotificationPayload.alertClass(forIdentifier: "somebody.else") == nil)
        #expect(AlertNotificationPayload.alertClass(forIdentifier: "capsule.alert.nope") == nil)
    }

    @Test("the payload reads back the alarm that was armed, re-wrap split included")
    func roundTrip() throws {
        let instant = day(30)
        let alarm = ArmedAlarm(
            alert: Alert(
                alertClass: .recoveryCheckDue,
                deadline: instant,
                params: ["recovery": "rewrap", "snooze_budget": "available"]
            ),
            fireAt: instant
        )
        var userInfo: [AnyHashable: Any] = [:]
        for (key, value) in AlertNotificationPayload.userInfo(for: alarm) {
            userInfo[key] = value
        }

        let decoded = try #require(AlertNotificationPayload.alarm(from: userInfo))
        #expect(decoded == alarm)
        #expect(decoded.alert.destination == .guidedRewrap)
    }

    @Test("a payload that is not ours decodes to nothing")
    func foreignPayload() {
        #expect(AlertNotificationPayload.alarm(from: ["aps": "x"]) == nil)
        #expect(AlertNotificationPayload.alarm(from: ["capsule.alert.class": "sync_stale"]) == nil)
    }
}
