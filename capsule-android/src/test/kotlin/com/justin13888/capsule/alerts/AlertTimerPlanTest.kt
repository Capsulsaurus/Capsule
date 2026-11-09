package com.justin13888.capsule.alerts

import java.time.Instant
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AlertTimerPlanTest {
    private fun alarm(
        alertClass: AlertClass,
        at: Instant,
        count: String? = null,
    ) = ArmedAlarm(Alert(alertClass, at, count?.let { mapOf("count" to it) } ?: emptyMap()), at)

    @Test
    fun armsAWantedClassThatHoldsNothing() {
        val wanted = alarm(AlertClass.SYNC_STALE, day(14))
        assertEquals(listOf<AlertTimerOperation>(AlertTimerOperation.Arm(wanted)), AlertTimerPlan.reconcile(listOf(wanted), emptyMap()))
    }

    @Test
    fun cancelsAnUnwantedClass() {
        val armed = alarm(AlertClass.RECOVERY_CHECK_DUE, day(90))
        assertEquals(
            listOf<AlertTimerOperation>(AlertTimerOperation.Cancel(AlertClass.RECOVERY_CHECK_DUE)),
            AlertTimerPlan.reconcile(emptyList(), mapOf(AlertClass.RECOVERY_CHECK_DUE to armed)),
        )
    }

    @Test
    fun reArmsAMovedDeadlineOrChangedCopy() {
        val old = alarm(AlertClass.SYNC_STALE, day(14), "2")
        for (new in listOf(alarm(AlertClass.SYNC_STALE, day(20), "2"), alarm(AlertClass.SYNC_STALE, day(14), "5"))) {
            assertEquals(
                listOf(AlertTimerOperation.Cancel(AlertClass.SYNC_STALE), AlertTimerOperation.Arm(new)),
                AlertTimerPlan.reconcile(listOf(new), mapOf(AlertClass.SYNC_STALE to old)),
            )
        }
    }

    @Test
    fun leavesAnEqualAlarmAlone() {
        val same = alarm(AlertClass.SYNC_STALE, day(14))
        assertTrue(AlertTimerPlan.reconcile(listOf(same), mapOf(AlertClass.SYNC_STALE to same)).isEmpty())
    }

    @Test
    fun aDuplicatedClassArmsOnce() {
        val first = alarm(AlertClass.SYNC_STALE, day(14))
        val second = alarm(AlertClass.SYNC_STALE, day(15))
        assertEquals(
            listOf<AlertTimerOperation>(AlertTimerOperation.Arm(second)),
            AlertTimerPlan.reconcile(listOf(first, second), emptyMap()),
        )
    }
}

class AlertModelTest {
    @Test
    fun wireNamesAreTheCores() {
        assertEquals(
            listOf("sync_stale", "recovery_check_due", "quota_soft", "quota_grace_expiring", "quarantine_pending", "drop_pending"),
            AlertClass.entries.map { it.wire },
        )
        assertEquals(listOf(AlertClass.SYNC_STALE, AlertClass.RECOVERY_CHECK_DUE), AlertClass.entries.filter { it.isPreArmable })
        assertNull(AlertClass.fromWire("nope"))
    }

    @Test
    fun rewrapIsNotTheRoutineCheck() {
        val check = Alert(AlertClass.RECOVERY_CHECK_DUE, params = mapOf("recovery" to "check"))
        val rewrap = Alert(AlertClass.RECOVERY_CHECK_DUE, params = mapOf("recovery" to "rewrap"))
        assertEquals(AlertDestination.RECOVERY_VERIFICATION, check.destination)
        assertEquals(AlertDestination.GUIDED_REWRAP, rewrap.destination)
        assertEquals("notification.recovery_check_due.check.title", check.copy.titleKey)
        assertEquals("notification.recovery_check_due.rewrap.body", rewrap.copy.bodyKey)
    }

    @Test
    fun syncStaleCopyCarriesTheCountAndDays() {
        val copy = Alert(AlertClass.SYNC_STALE, params = mapOf("count" to "7", "days_behind" to "15")).copy
        assertEquals(AlertCopy("notification.sync_stale", 7, listOf("15")), copy)
    }

    @Test
    fun graceSelectsItsKey() {
        assertEquals(
            "notification.quota_grace_expiring.expired",
            Alert(AlertClass.QUOTA_GRACE_EXPIRING, params = mapOf("grace" to "expired")).copy.stem,
        )
        assertEquals("notification.quota_grace_expiring.counting", Alert(AlertClass.QUOTA_GRACE_EXPIRING).copy.stem)
    }

    @Test
    fun anArmedSyncAlarmCarriesTheDaysToItsDeadline() {
        val facts = NotifyFacts(lastCompletedSync = EPOCH, unsyncedChanges = 4)
        val alert = facts.armedAlert(ClassDeadline(AlertClass.SYNC_STALE, day(14)))
        assertEquals(mapOf("count" to "4", "days_behind" to "14"), alert.params)
    }
}
