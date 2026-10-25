import CapsuleDomain
import CapsulePorts
import Testing

@testable import CapsuleMock

/// The mock lane's predicate must agree with the core's documented rules, or
/// the mock lane would rehearse a lifecycle the real app does not have.
@Suite("The mock alert policy follows the core's rules")
struct MockAlertPolicyTests {
    private let policy = MockAlertPolicy()
    private let last = CapsuleTimestamp(epochSeconds: 1767225600)

    private func at(days: Int64, seconds: Int64 = 0) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: last.epochSeconds + days * 86400 + seconds)
    }

    @Test("sync_stale fires at the two-week boundary instant, not a second later")
    func boundary() throws {
        let input = NotifyInput(lastCompletedSync: last, unsyncedChanges: 1)
        #expect(try policy.evaluate(input, at: at(days: 14, seconds: -1)).isEmpty)
        let fired = try policy.evaluate(input, at: at(days: 14))
        #expect(fired.map(\.alertClass) == [.syncStale])
        #expect(fired.first?.params["days_behind"] == "14")
    }

    @Test("nothing un-synced raises nothing and arms nothing")
    func nothingUnsynced() throws {
        let input = NotifyInput(lastCompletedSync: last, unsyncedChanges: 0)
        #expect(try policy.evaluate(input, at: at(days: 30)).isEmpty)
        #expect(try policy.preArmDeadlines(input, at: at(days: 1)).isEmpty)
    }

    @Test("a snooze defers the armed instant to its end; a disable removes it")
    func snoozeDefersDisableCancels() throws {
        var input = NotifyInput(lastCompletedSync: last, unsyncedChanges: 1)
        input.suppressedUntil[.syncStale] = at(days: 16)
        #expect(try policy.preArmDeadlines(input, at: at(days: 15)) == [
            ClassDeadline(alertClass: .syncStale, deadline: at(days: 16)),
        ])
        input.disabled = [.syncStale]
        #expect(try policy.preArmDeadlines(input, at: at(days: 15)).isEmpty)
    }

    @Test("a past deadline is not armed, and server-held classes never are")
    func onlyFutureAndPreArmable() throws {
        let input = NotifyInput(
            lastCompletedSync: last,
            unsyncedChanges: 1,
            quotaState: .graceExpired,
            quarantinePending: 3,
            dropsPending: 1
        )
        #expect(try policy.preArmDeadlines(input, at: at(days: 20)).isEmpty)
        let alerts = try policy.evaluate(input, at: at(days: 20))
        #expect(alerts.map(\.alertClass) == [.syncStale, .quotaGraceExpiring, .quarantinePending, .dropPending])
        #expect(alerts.first { $0.alertClass == .quotaGraceExpiring }?.params["grace"] == "expired")
    }

    @Test("a spent recovery budget is reported as a badge but never armed")
    func spentRecoveryBudget() throws {
        let input = NotifyInput(recoveryNextDue: at(days: 1), recoverySnoozeBudgetSpent: true)
        #expect(try policy.preArmDeadlines(input, at: last).isEmpty)
        let alert = try #require(try policy.evaluate(input, at: at(days: 2)).first)
        #expect(alert.isSnoozeBudgetSpent)
    }
}
