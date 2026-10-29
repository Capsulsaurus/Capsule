import CapsuleDomain
import Testing

@testable import CapsuleCatalogFFI

/// The Rust-backed alert predicate, called through the uniffi boundary.
///
/// What these pin is the adapter, not the predicate — `capsule-core`'s own
/// tests own the thresholds. Each case is one the conversion could get wrong:
/// an instant that crosses as the wrong string, a class or quota state mapped
/// to its neighbour, a parameter dropped, or the per-class deadlines collapsed.
@Suite("FFIAlertPolicy — the core's predicate over uniffi")
struct FFIAlertPolicyTests {
    private let policy = FFIAlertPolicy()
    private static let base: Int64 = 1_700_000_000
    private static let day: Int64 = 86400
    private static let syncStale: Int64 = 14 * day

    private func at(_ seconds: Int64) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: seconds)
    }

    @Test("sync_stale fires exactly at two weeks and carries its count and days")
    func syncStaleAtThreshold() throws {
        let input = NotifyInput(lastCompletedSync: at(Self.base), unsyncedChanges: 3)
        let due = Self.base + Self.syncStale

        #expect(try policy.evaluate(input, at: at(due - 1)).isEmpty)
        let alerts = try policy.evaluate(input, at: at(due))
        let alert = try #require(alerts.first)
        #expect(alerts.count == 1)
        #expect(alert.alertClass == .syncStale)
        #expect(alert.deadline == at(due))
        #expect(alert.params[Alert.Parameter.count] == "3")
        #expect(alert.params[Alert.Parameter.daysBehind] == "14")
    }

    @Test("both pre-armable classes keep their own deadline — never collapsed to the earliest")
    func perClassDeadlines() throws {
        let recoveryDue = Self.base + 90 * Self.day
        let input = NotifyInput(
            lastCompletedSync: at(Self.base),
            unsyncedChanges: 1,
            recoveryNextDue: at(recoveryDue)
        )

        let deadlines = try policy.preArmDeadlines(input, at: at(Self.base))

        #expect(Set(deadlines) == [
            ClassDeadline(alertClass: .syncStale, deadline: at(Self.base + Self.syncStale)),
            ClassDeadline(alertClass: .recoveryCheckDue, deadline: at(recoveryDue)),
        ])
    }

    @Test("a re-wrap escalation crosses as the recovery parameter, so the tap opens the re-wrap")
    func rewrapParameter() throws {
        let input = NotifyInput(recoveryNextDue: at(Self.base), recoveryRewrapDue: true)

        let alert = try #require(try policy.evaluate(input, at: at(Self.base)).first)

        #expect(alert.alertClass == .recoveryCheckDue)
        #expect(alert.isGuidedRewrap)
        #expect(alert.destination == .guidedRewrap)
    }

    @Test("every known quota state maps to the core's advisory; an unknown one raises nothing")
    func quotaStates() throws {
        let soft = try policy.evaluate(NotifyInput(quotaState: .softWarning), at: at(Self.base))
        #expect(soft.map(\.alertClass) == [.quotaSoft])

        let expired = try policy.evaluate(NotifyInput(quotaState: .graceExpired), at: at(Self.base))
        #expect(expired.map(\.alertClass) == [.quotaGraceExpiring])
        #expect(expired.first?.params[Alert.Parameter.grace] == "expired")

        #expect(try policy.evaluate(NotifyInput(quotaState: .withinQuota), at: at(Self.base)).isEmpty)
        #expect(try policy.evaluate(NotifyInput(quotaState: .unknown("future_state")), at: at(Self.base)).isEmpty)
        #expect(FFIAlertPolicy.quotaAdvisory(.unknown("future_state")) == nil)
    }

    @Test("a snoozed class defers its alarm to the snooze end; a disabled one holds none")
    func snoozeAndDisable() throws {
        let snoozeEnd = Self.base + Self.syncStale + Self.day
        let snoozed = NotifyInput(
            lastCompletedSync: at(Self.base),
            unsyncedChanges: 1,
            suppressedUntil: [.syncStale: at(snoozeEnd)]
        )
        #expect(try policy.preArmDeadlines(snoozed, at: at(Self.base)) == [
            ClassDeadline(alertClass: .syncStale, deadline: at(snoozeEnd)),
        ])

        let disabled = NotifyInput(lastCompletedSync: at(Self.base), unsyncedChanges: 1, disabled: [.syncStale])
        #expect(try policy.preArmDeadlines(disabled, at: at(Self.base)).isEmpty)
        #expect(try policy.evaluate(disabled, at: at(Self.base + Self.syncStale)).isEmpty)
    }

    @Test("counted server-held classes carry their count and are never pre-armed")
    func countedClasses() throws {
        let input = NotifyInput(quarantinePending: 2, dropsPending: 5)

        let alerts = try policy.evaluate(input, at: at(Self.base))

        #expect(Set(alerts.map(\.alertClass)) == [.quarantinePending, .dropPending])
        #expect(alerts.first { $0.alertClass == .dropPending }?.params[Alert.Parameter.count] == "5")
        #expect(try policy.preArmDeadlines(input, at: at(Self.base)).isEmpty)
    }

    @Test("a negative count is clamped, not trapped, at the unsigned boundary")
    func negativeCountClamps() {
        let ffi = FFIAlertPolicy.ffi(NotifyInput(unsyncedChanges: -4, quarantinePending: -1, dropsPending: -9))
        #expect(ffi.unsyncedChanges == 0)
        #expect(ffi.quarantinePending == 0)
        #expect(ffi.dropsPending == 0)
    }

    @Test("an instant the core renders that does not parse is reported, never read as the epoch")
    func malformedOutputThrows() {
        #expect(throws: FFIAlertPolicyError.malformedOutput(field: "deadline", value: "soon")) {
            try FFIAlertPolicy.instant("soon", field: "deadline")
        }
    }
}
