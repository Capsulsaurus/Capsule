import CapsuleDomain
import CapsuleMock
import CapsulePorts
import Testing

@testable import CapsuleAlerts

/// The client's own state, wired into the predicate's input from the ports
/// that already hold it.
@Suite("Alert facts are read from the existing ports")
struct PortAlertFactsTests {
    private func facts(_ scenario: MockScenario) async -> (NotifyInput, MockEnvironment) {
        let environment = MockEnvironment(scenario: scenario)
        let facts = PortAlertFacts(
            sync: environment.sync,
            quota: environment.quota,
            quarantine: environment.quarantine,
            drops: environment.drops,
            recovery: environment.recovery,
            settings: environment.settings
        )
        return await (facts.facts(), environment)
    }

    @Test("sync facts come from the sync status")
    func syncFacts() async throws {
        let (input, environment) = await facts(.healthy)
        let status = try await environment.sync.status()
        #expect(input.lastCompletedSync == status.lastCompletedSyncAt)
        #expect(input.unsyncedChanges == status.pendingUploadCount)
    }

    @Test("the quarantine count excludes pending drops, which have their own class")
    func quarantineExcludesDrops() async throws {
        let (input, environment) = await facts(.quarantine)
        let held = try await environment.quarantine.itemCount()
        let drops = try await environment.quarantine.items(
            on: .pendingDropAwaitingAdoption,
            offset: 0,
            limit: PageRequest.defaultLimit
        )
        #expect(input.quarantinePending == held - (drops.totalCount ?? drops.items.count))
        #expect(input.quarantinePending > 0)
    }

    @Test("an overdue recovery with its snoozes spent reports the re-wrap, and is never armed")
    func recoveryOverdue() async throws {
        let (input, environment) = await facts(.recoveryOverdue)
        #expect(input.recoveryNextDue != nil)
        #expect(input.recoverySnoozeBudgetSpent)
        #expect(input.recoveryRewrapDue)

        let now = environment.configuration.clock.now
        let policy = MockAlertPolicy()
        let recovery = try #require(try policy.evaluate(input, at: now).first { $0.alertClass == .recoveryCheckDue })
        #expect(recovery.isGuidedRewrap)
        #expect(recovery.isSnoozeBudgetSpent)
        #expect(try policy.preArmDeadlines(input, at: now).allSatisfy { $0.alertClass != .recoveryCheckDue })
    }

    @Test("a turned-off staleness warning disables sync_stale and nothing else")
    func stalenessSwitch() async throws {
        let environment = MockEnvironment(scenario: .healthy)
        var settings = try await environment.settings.settings()
        settings.stalenessNotificationEnabled = false
        try await environment.settings.update(settings)
        let facts = PortAlertFacts(
            sync: environment.sync,
            quota: environment.quota,
            quarantine: environment.quarantine,
            drops: environment.drops,
            recovery: environment.recovery,
            settings: environment.settings
        )

        let input = await facts.facts()

        #expect(input.disabled == [.syncStale])
    }
}
