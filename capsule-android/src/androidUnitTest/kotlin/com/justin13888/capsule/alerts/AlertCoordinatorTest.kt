package com.justin13888.capsule.alerts

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The pre-arm lifecycle under a mocked clock (Notifications - Validation): arm on
 * deadline-known, re-arm on the change that moves it, cancel when it clears, never two
 * live alarms per class; the snooze bound degrading to a badge; a refused permission
 * degrading to a badge without error.
 */
class AlertCoordinatorTest {
    @Test
    fun armsTwoWeeksAfterTheLastSync() {
        val harness = Harness(NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 3), now = day(1))

        val outcome = harness.coordinator.refresh()

        assertEquals(AlertRefreshOutcome.Delivery.ARMED, outcome.delivery)
        assertEquals(mapOf(AlertClass.SYNC_STALE to day(14)), harness.armedAt())
        assertEquals(
            mapOf("count" to "3", "days_behind" to "14"),
            harness.scheduler.armed().getValue(AlertClass.SYNC_STALE).alert.params,
        )
    }

    @Test
    fun aCompletedSyncMovesTheOneAlarm() {
        val harness = Harness(NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 3), now = day(1))
        harness.coordinator.refresh()

        harness.facts = NotifyFacts(lastCompletedSync = day(5), unsyncedChanges = 1)
        harness.now = day(5, plusSeconds = 60)
        val outcome = harness.coordinator.refresh()

        assertEquals(mapOf(AlertClass.SYNC_STALE to day(19)), harness.armedAt())
        assertEquals(AlertTimerOperation.Cancel(AlertClass.SYNC_STALE), outcome.operations.first())
        assertEquals(1, harness.scheduler.maximumLivePerClass())
    }

    @Test
    fun cancelsWhenTheConditionClears() {
        val harness = Harness(NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 3), now = day(1))
        harness.coordinator.refresh()

        harness.facts = NotifyFacts(lastCompletedSync = day(2), unsyncedChanges = 0)
        val outcome = harness.coordinator.refresh()

        assertEquals(AlertRefreshOutcome.Delivery.NOTHING_TO_ARM, outcome.delivery)
        assertTrue(harness.armedAt().isEmpty())
    }

    @Test
    fun eachPreArmableClassHoldsItsOwnAlarm() {
        val facts = NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 2, recoveryNextDue = day(90))
        val harness = Harness(facts, now = day(1))

        harness.coordinator.refresh()

        assertEquals(mapOf(AlertClass.SYNC_STALE to day(14), AlertClass.RECOVERY_CHECK_DUE to day(90)), harness.armedAt())
    }

    @Test
    fun neverTwoLiveAlarmsPerClassAcrossAMonth() {
        val harness = Harness()
        for (step in 0L until 30L) {
            harness.now = day(step, plusSeconds = 1)
            harness.facts =
                NotifyFacts(
                    lastCompletedSync = day(step / 3 * 3),
                    unsyncedChanges = step % 4,
                    recoveryNextDue = day(30 + step % 2),
                )
            harness.coordinator.refresh()
            assertTrue(harness.scheduler.armed().size <= 2)
        }
        assertEquals(1, harness.scheduler.maximumLivePerClass())
    }

    @Test
    fun aRefreshThatChangedNothingTouchesNothing() {
        val harness = Harness(NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 3), now = day(1))
        harness.coordinator.refresh()

        assertTrue(harness.coordinator.refresh().operations.isEmpty())
        assertEquals(1, harness.scheduler.log.size)
    }

    @Test
    fun permissionIsAskedOnlyWhenSomethingNeedsArming() {
        val permission = FakePermission()
        val harness = Harness(NotifyFacts(quarantinePending = 2), now = day(1), permission = permission)

        val first = harness.coordinator.refresh()
        assertEquals(AlertRefreshOutcome.Delivery.NOTHING_TO_ARM, first.delivery)
        assertEquals(listOf(AlertClass.QUARANTINE_PENDING), first.badges.map { it.alertClass })
        assertEquals(0, permission.requests)

        harness.facts = NotifyFacts(lastCompletedSync = day(1), unsyncedChanges = 1, quarantinePending = 2)
        val asking = harness.coordinator.refresh()
        assertEquals(AlertRefreshOutcome.Delivery.AWAITING_PERMISSION, asking.delivery)
        assertEquals(1, permission.requests)

        permission.current = NotificationPermissionState.GRANTED
        harness.coordinator.refresh()
        assertEquals(mapOf(AlertClass.SYNC_STALE to day(15)), harness.armedAt())
        assertEquals(1, permission.requests)
    }

    @Test
    fun aRefusalDegradesToBadgesWithoutRetry() {
        val armed = ArmedAlarm(Alert(AlertClass.SYNC_STALE), day(14))
        val permission = FakePermission(NotificationPermissionState.DENIED)
        val harness =
            Harness(
                NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 3, quotaState = QuotaAdvisory.SOFT_WARNING),
                now = day(1),
                permission = permission,
                scheduler = FakeScheduler(listOf(armed)),
            )

        val outcome = harness.coordinator.refresh()

        assertEquals(AlertRefreshOutcome.Delivery.BADGES_ONLY, outcome.delivery)
        assertEquals(listOf(AlertClass.QUOTA_SOFT), outcome.badges.map { it.alertClass })
        assertEquals(listOf<AlertTimerOperation>(AlertTimerOperation.Cancel(AlertClass.SYNC_STALE)), outcome.operations)
        assertEquals(0, permission.requests)
    }

    @Test
    fun theSnoozeIsBoundedThenABadge() {
        val harness = Harness(NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 3), now = day(15))
        assertEquals(listOf(AlertClass.SYNC_STALE), harness.coordinator.refresh().badges.map { it.alertClass })

        repeat(AlertSnoozeLedger.MAXIMUM_CONSECUTIVE_SNOOZES - 1) {
            val end = harness.now.plusSeconds(AlertSnoozeLedger.INTERVAL_SECONDS)
            assertEquals(AlertSnoozeOutcome.Snoozed(end), harness.coordinator.snooze(AlertClass.SYNC_STALE))
            assertEquals(mapOf(AlertClass.SYNC_STALE to end), harness.armedAt())
            assertTrue(harness.coordinator.badges.isEmpty())
            harness.now = end.plusSeconds(1)
            harness.coordinator.refresh()
        }

        harness.coordinator.snooze(AlertClass.SYNC_STALE)
        assertTrue(harness.armedAt().isEmpty())
        assertEquals(AlertSnoozeOutcome.BadgeOnly, harness.coordinator.snooze(AlertClass.SYNC_STALE))
        harness.now = harness.now.plusSeconds(2 * AlertSnoozeLedger.INTERVAL_SECONDS)
        assertEquals(listOf(AlertClass.SYNC_STALE), harness.coordinator.refresh().badges.map { it.alertClass })
        assertTrue(harness.armedAt().isEmpty())
    }

    @Test
    fun aClearedClassGetsAFreshBudget() {
        val harness = Harness(NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 3), now = day(15))
        repeat(AlertSnoozeLedger.MAXIMUM_CONSECUTIVE_SNOOZES) { harness.coordinator.snooze(AlertClass.SYNC_STALE) }

        harness.facts = NotifyFacts(lastCompletedSync = day(15), unsyncedChanges = 0)
        harness.coordinator.refresh()

        assertTrue(harness.snoozes.ledger.entries.isEmpty())
    }

    @Test
    fun theRecoveryCheckIsSnoozedByItsOwnCadence() {
        val harness = Harness(NotifyFacts(recoveryNextDue = EPOCH), now = day(1))
        assertEquals(AlertSnoozeOutcome.OwnedByRecoveryCadence, harness.coordinator.snooze(AlertClass.RECOVERY_CHECK_DUE))
    }

    @Test
    fun aRewrapAlarmOpensTheRewrap() {
        val harness = Harness(NotifyFacts(recoveryNextDue = day(10), recoveryRewrapDue = true), now = day(1))

        harness.coordinator.refresh()

        val alarm = harness.scheduler.armed().getValue(AlertClass.RECOVERY_CHECK_DUE)
        assertEquals(AlertDestination.GUIDED_REWRAP, alarm.alert.destination)
        assertEquals("notification.recovery_check_due.rewrap.title", alarm.alert.copy.titleKey)
    }

    @Test
    fun aRefusedInputLeavesTheAlarmsAsTheyWere() {
        val armed = ArmedAlarm(Alert(AlertClass.SYNC_STALE), day(14))
        val harness = Harness(scheduler = FakeScheduler(listOf(armed)))
        harness.policy.refuse = true

        val outcome = harness.coordinator.refresh()

        assertEquals(AlertRefreshOutcome.Delivery.POLICY_FAILED, outcome.delivery)
        assertEquals(mapOf(AlertClass.SYNC_STALE to day(14)), harness.armedAt())
    }
}
