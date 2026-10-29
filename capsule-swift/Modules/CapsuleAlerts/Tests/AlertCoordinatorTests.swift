import CapsuleDomain
import CapsuleMock
import CapsulePorts
import Foundation
import Testing

@testable import CapsuleAlerts

/// The pre-arm lifecycle under a mocked clock (*Notifications — Validation*):
/// arm on deadline-known, re-arm on the change that moves it, cancel when it
/// clears, never two live timers per class; the snooze bound degrading to a
/// badge; and a refused authorization degrading to a badge without error.
@Suite("The alert loop arms, re-arms and cancels one alarm per class")
struct AlertCoordinatorTests {
    // MARK: Lifecycle

    @Test("a known sync deadline is armed for two weeks after the last sync")
    func armsOnDeadlineKnown() async {
        let harness = Harness(NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3), at: day(1))

        let outcome = await harness.coordinator.refresh()

        #expect(outcome.delivery == .armed)
        #expect(await harness.armedInstants() == [.syncStale: day(14)])
        let alarm = await harness.scheduler.armed()[.syncStale]
        #expect(alarm?.alert.params == ["count": "3", "days_behind": "14"])
    }

    @Test("a completed sync moves the alarm rather than adding a second one")
    func reArmsOnChange() async {
        let harness = Harness(NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3), at: day(1))
        await harness.coordinator.refresh()

        await harness.facts.set(NotifyInput(lastCompletedSync: day(5), unsyncedChanges: 1))
        harness.clock.set(day(5, plusSeconds: 60))
        let outcome = await harness.coordinator.refresh()

        #expect(await harness.armedInstants() == [.syncStale: day(19)])
        #expect(outcome.operations.count == 2)
        #expect(outcome.operations.first == .cancel(.syncStale))
        #expect(await harness.maximumLivePerClass() == 1)
    }

    @Test("a refresh that changed nothing touches nothing")
    func idempotent() async {
        let harness = Harness(NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3), at: day(1))
        await harness.coordinator.refresh()

        let again = await harness.coordinator.refresh()

        #expect(again.operations.isEmpty)
        #expect(await harness.scheduler.log.count == 1)
    }

    @Test("the alarm is cancelled when its condition clears")
    func cancelsWhenCleared() async {
        let harness = Harness(NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3), at: day(1))
        await harness.coordinator.refresh()

        await harness.facts.set(NotifyInput(lastCompletedSync: day(2), unsyncedChanges: 0))
        let outcome = await harness.coordinator.refresh()

        #expect(outcome.delivery == .nothingToArm)
        #expect(await harness.armedInstants().isEmpty)
        #expect(await harness.scheduler.log.last == .cancelled(.syncStale))
    }

    @Test("each pre-armable class holds its own alarm — the later one is not lost to the earlier")
    func armsPerClass() async {
        let input = NotifyInput(
            lastCompletedSync: epoch,
            unsyncedChanges: 2,
            recoveryNextDue: day(90)
        )
        let harness = Harness(input, at: day(1))

        await harness.coordinator.refresh()

        #expect(await harness.armedInstants() == [.syncStale: day(14), .recoveryCheckDue: day(90)])
    }

    @Test("across a month of changes no class ever holds two live alarms")
    func neverTwoLiveTimers() async {
        let harness = Harness(NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 1, recoveryNextDue: day(30)))
        for step in Int64(0) ..< 30 {
            harness.clock.set(day(step, plusSeconds: 1))
            await harness.facts.set(NotifyInput(
                lastCompletedSync: day(step / 3 * 3),
                unsyncedChanges: Int(step % 4),
                recoveryNextDue: day(30 + step % 2)
            ))
            await harness.coordinator.refresh()
            #expect(await harness.scheduler.armed().count <= 2)
        }
        #expect(await harness.maximumLivePerClass() == 1)
    }

    @Test("a disabled class reports nothing and holds no alarm")
    func disabledHoldsNothing() async {
        let input = NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3, disabled: [.syncStale])
        let harness = Harness(input, at: day(20))

        let outcome = await harness.coordinator.refresh()

        #expect(outcome.badges.isEmpty)
        #expect(await harness.armedInstants().isEmpty)
    }

    @Test("an armed re-wrap alarm opens the re-wrap, not the routine check")
    func rewrapAlarmRoutesToRewrap() async throws {
        let harness = Harness(NotifyInput(recoveryNextDue: day(10), recoveryRewrapDue: true), at: day(1))

        await harness.coordinator.refresh()

        let alarm = try #require(await harness.scheduler.armed()[.recoveryCheckDue])
        #expect(alarm.alert.destination == .guidedRewrap)
        #expect(alarm.alert.copy.titleKey == "notification.recovery_check_due.rewrap.title")
    }

    // MARK: Permission

    @Test("permission is asked the first time something needs arming, and only then")
    func permissionAtFirstUse() async {
        let harness = Harness(NotifyInput(quarantinePending: 2), at: day(1))

        // Only a server-held class is true: nothing to arm, so nothing to ask.
        let first = await harness.coordinator.refresh()
        #expect(first.delivery == .nothingToArm)
        #expect(first.badges.map(\.alertClass) == [.quarantinePending])
        #expect(await harness.authorization.requestCount == 0)

        await harness.facts.set(NotifyInput(lastCompletedSync: day(1), unsyncedChanges: 1, quarantinePending: 2))
        await harness.coordinator.refresh()
        await harness.coordinator.refresh()

        #expect(await harness.authorization.requestCount == 1)
        #expect(await harness.armedInstants() == [.syncStale: day(15)])
    }

    @Test("a refused prompt degrades every class to its badge, without error or retry")
    func denialDegradesToBadges() async {
        let harness = Harness(
            NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3, quotaState: .softWarning),
            at: day(1),
            authorization: MockNotificationAuthorization(status: .notDetermined, answer: .denied)
        )

        let outcome = await harness.coordinator.refresh()
        await harness.coordinator.refresh()

        #expect(outcome.delivery == .badgesOnly)
        #expect(outcome.badges.map(\.alertClass) == [.quotaSoft])
        #expect(await harness.armedInstants().isEmpty)
        #expect(await harness.authorization.requestCount == 1)
    }

    @Test("revoking permission cancels the alarms already armed")
    func revokedPermissionCancels() async {
        let armed = ArmedAlarm(alert: Alert(alertClass: .syncStale), fireAt: day(14))
        let harness = Harness(
            NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3),
            at: day(1),
            authorization: MockNotificationAuthorization(status: .denied),
            scheduler: InMemoryAlertScheduler(armed: [armed])
        )

        let outcome = await harness.coordinator.refresh()

        #expect(outcome.delivery == .badgesOnly)
        #expect(outcome.operations == [.cancel(.syncStale)])
    }

    // MARK: Snooze

    @Test("a snooze defers the fired alert to the snooze end, then the budget degrades it to a badge")
    func snoozeIsBoundedThenBadge() async {
        let input = NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3)
        let harness = Harness(input, at: day(15))

        // Already past its deadline: shown, nothing left to arm.
        let fired = await harness.coordinator.refresh()
        #expect(fired.badges.map(\.alertClass) == [.syncStale])
        #expect(await harness.armedInstants().isEmpty)

        // Each snooze hides it and re-arms it for the snooze end.
        for snooze in 1 ..< AlertSnoozeLedger.maximumConsecutiveSnoozes {
            let outcome = await harness.coordinator.snooze(.syncStale)
            let end = CapsuleTimestamp(epochSeconds: harness.clock.now.epochSeconds + AlertSnoozeLedger.interval)
            #expect(outcome == .snoozed(until: end), "snooze \(snooze)")
            #expect(await harness.armedInstants() == [.syncStale: end])
            #expect(await harness.coordinator.badges.isEmpty)
            harness.clock.set(CapsuleTimestamp(epochSeconds: end.epochSeconds + 1))
            await harness.coordinator.refresh()
        }

        // The last snooze in the budget is honoured but arms nothing after it…
        _ = await harness.coordinator.snooze(.syncStale)
        #expect(await harness.armedInstants().isEmpty)
        // …and once the budget is spent the class is a badge that stays a badge.
        #expect(await harness.coordinator.snooze(.syncStale) == .badgeOnly)
        harness.clock.set(CapsuleTimestamp(epochSeconds: harness.clock.now.epochSeconds + 2 * AlertSnoozeLedger.interval))
        let badge = await harness.coordinator.refresh()
        #expect(badge.badges.map(\.alertClass) == [.syncStale])
        #expect(await harness.armedInstants().isEmpty)
    }

    @Test("a class whose condition clears gets a fresh snooze budget")
    func clearingResetsBudget() async {
        let harness = Harness(NotifyInput(lastCompletedSync: epoch, unsyncedChanges: 3), at: day(15))
        for _ in 0 ..< AlertSnoozeLedger.maximumConsecutiveSnoozes {
            await harness.coordinator.snooze(.syncStale)
        }
        #expect(await harness.coordinator.snooze(.syncStale) == .badgeOnly)

        await harness.facts.set(NotifyInput(lastCompletedSync: day(15), unsyncedChanges: 0))
        await harness.coordinator.refresh()

        #expect(harness.snoozes.load().entries.isEmpty)
    }

    @Test("the recovery check is snoozed by its own cadence, not by the ledger")
    func recoverySnoozeIsNotOwnedHere() async {
        let harness = Harness(NotifyInput(recoveryNextDue: epoch), at: day(1))
        #expect(await harness.coordinator.snooze(.recoveryCheckDue) == .ownedByRecoveryCadence)
        #expect(harness.snoozes.load().entries.isEmpty)
    }

    // MARK: Failure

    @Test("a predicate that refuses its input leaves the armed alarms as they were")
    func policyFailureKeepsAlarms() async {
        let armed = ArmedAlarm(alert: Alert(alertClass: .syncStale), fireAt: day(14))
        let harness = Harness(
            NotifyInput(),
            scheduler: InMemoryAlertScheduler(armed: [armed]),
            policy: RefusingPolicy()
        )

        let outcome = await harness.coordinator.refresh()

        #expect(outcome.delivery == .policyFailed)
        #expect(await harness.armedInstants() == [.syncStale: day(14)])
    }
}
