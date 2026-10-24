import Foundation
import Testing

import CapsuleDomain

/// The alert vocabulary: wire names, catalog keys, and where a tap leads
/// (*Notifications — Alert Classes*).
@Suite("Alert classes map to their catalog copy and destinations")
struct AlertsTests {
    @Test("wire names are the core's, and only two classes are pre-armable")
    func wireNames() {
        #expect(AlertClass.allCases.map(\.rawValue) == [
            "sync_stale", "recovery_check_due", "quota_soft",
            "quota_grace_expiring", "quarantine_pending", "drop_pending",
        ])
        #expect(AlertClass.allCases.filter(\.isPreArmable) == [.syncStale, .recoveryCheckDue])
    }

    @Test("severity follows the core's per-class assignment")
    func severity() {
        let warnings = AlertClass.allCases.filter { $0.severity == .warning }
        #expect(warnings == [.syncStale, .quotaGraceExpiring, .quarantinePending])
    }

    @Test("sync_stale renders its count as the plural and days behind as text")
    func syncStaleCopy() {
        let copy = Alert(alertClass: .syncStale, params: ["count": "7", "days_behind": "15"]).copy
        #expect(copy.titleKey == "notification.sync_stale.title")
        #expect(copy.bodyKey == "notification.sync_stale.body")
        #expect(copy.count == 7)
        #expect(copy.textArguments == ["15"])
    }

    @Test("recovery = rewrap selects the re-wrap copy and destination, never the check")
    func rewrapIsDistinct() {
        let check = Alert(alertClass: .recoveryCheckDue, params: ["recovery": "check", "snooze_budget": "available"])
        let rewrap = Alert(alertClass: .recoveryCheckDue, params: ["recovery": "rewrap", "snooze_budget": "available"])

        #expect(check.copy.titleKey == "notification.recovery_check_due.check.title")
        #expect(check.destination == .recoveryVerification)
        #expect(!check.isGuidedRewrap)

        #expect(rewrap.copy.titleKey == "notification.recovery_check_due.rewrap.title")
        #expect(rewrap.copy.bodyKey == "notification.recovery_check_due.rewrap.body")
        #expect(rewrap.destination == .guidedRewrap)
        #expect(rewrap.isGuidedRewrap)
    }

    @Test("the grace parameter selects the counting or expired copy")
    func graceCopy() {
        let counting = Alert(alertClass: .quotaGraceExpiring, params: ["grace": "counting"]).copy
        let expired = Alert(alertClass: .quotaGraceExpiring, params: ["grace": "expired"]).copy
        #expect(counting.titleKey == "notification.quota_grace_expiring.counting.title")
        #expect(expired.titleKey == "notification.quota_grace_expiring.expired.title")
        #expect(counting.count == nil)
    }

    @Test("every class resolves to a notification.* title and body key")
    func everyClassHasKeys() {
        for alertClass in AlertClass.allCases {
            let copy = Alert(alertClass: alertClass).copy
            #expect(copy.titleKey.hasPrefix("notification.\(alertClass.rawValue)"))
            #expect(copy.titleKey.hasSuffix(".title"))
            #expect(copy.bodyKey.hasSuffix(".body"))
        }
    }

    @Test("an armed sync_stale alarm carries the current count and the days to its deadline")
    func armedSyncStale() {
        let input = NotifyInput(lastCompletedSync: Fixtures.epoch, unsyncedChanges: 4)
        let deadline = Fixtures.time(offsetSeconds: 14 * 86400)
        let alert = input.armedAlert(for: ClassDeadline(alertClass: .syncStale, deadline: deadline))
        #expect(alert.params == ["count": "4", "days_behind": "14"])
        #expect(alert.deadline == deadline)
    }

    @Test("an armed recovery alarm carries the re-wrap split and an available budget")
    func armedRecovery() {
        let input = NotifyInput(recoveryNextDue: Fixtures.epoch, recoveryRewrapDue: true)
        let alert = input.armedAlert(for: ClassDeadline(alertClass: .recoveryCheckDue, deadline: Fixtures.epoch))
        #expect(alert.params == ["recovery": "rewrap", "snooze_budget": "available"])
        #expect(alert.destination == .guidedRewrap)
        #expect(!alert.isSnoozeBudgetSpent)
    }
}
