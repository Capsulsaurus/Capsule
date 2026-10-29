package com.justin13888.capsule

import com.justin13888.capsule.alerts.Alert
import com.justin13888.capsule.alerts.AlertClass
import com.justin13888.capsule.alerts.AlertDestination
import com.justin13888.capsule.alerts.AlertParameter
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class AlertTapTest {
    @Test
    fun everyArmedDestinationDecodesToItself() {
        AlertDestination.entries.forEach { destination ->
            assertEquals(destination, tappedAlertDestination(destination.name))
        }
    }

    @Test
    fun aRewrapAlertTapOpensTheRewrapNeverTheRoutineCheck() {
        val rewrap = Alert(AlertClass.RECOVERY_CHECK_DUE, params = mapOf(AlertParameter.RECOVERY to "rewrap"))
        val check = Alert(AlertClass.RECOVERY_CHECK_DUE, params = mapOf(AlertParameter.RECOVERY to "check"))

        assertEquals(AlertDestination.GUIDED_REWRAP, tappedAlertDestination(rewrap.destination.name))
        assertEquals(AlertDestination.RECOVERY_VERIFICATION, tappedAlertDestination(check.destination.name))
    }

    @Test
    fun anIntentThatIsNotAnAlertTapRoutesNowhere() {
        assertNull(tappedAlertDestination(null))
        assertNull(tappedAlertDestination("SOMEWHERE_NEW"))
        assertNull(tappedAlertDestination("guided_rewrap"))
    }
}
