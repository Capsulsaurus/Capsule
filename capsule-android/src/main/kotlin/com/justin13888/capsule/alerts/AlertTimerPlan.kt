package com.justin13888.capsule.alerts

/** One change to the OS's armed alarms. */
sealed interface AlertTimerOperation {
    /** Cancel whatever the class holds. */
    data class Cancel(
        val alertClass: AlertClass,
    ) : AlertTimerOperation

    /** Arm this alarm, replacing whatever its class holds. */
    data class Arm(
        val alarm: ArmedAlarm,
    ) : AlertTimerOperation
}

/**
 * The arm / re-arm / cancel rule as a pure function: wanted and not armed is armed;
 * armed and not wanted is cancelled; armed differently (another instant or other copy)
 * is cancelled then armed, so its one alarm moves rather than a second appearing;
 * armed exactly as wanted is left alone. Operations come out in [AlertClass] order.
 */
object AlertTimerPlan {
    fun reconcile(
        desired: List<ArmedAlarm>,
        armed: Map<AlertClass, ArmedAlarm>,
    ): List<AlertTimerOperation> {
        // Last entry per class wins, so a duplicated input never arms a class twice.
        val wanted = desired.associateBy { it.alertClass }
        val operations = mutableListOf<AlertTimerOperation>()
        for (alertClass in AlertClass.entries) {
            val current = armed[alertClass]
            val alarm = wanted[alertClass]
            when {
                current == null && alarm == null -> Unit
                alarm == null -> operations += AlertTimerOperation.Cancel(alertClass)
                current == null -> operations += AlertTimerOperation.Arm(alarm)
                current != alarm -> {
                    operations += AlertTimerOperation.Cancel(alertClass)
                    operations += AlertTimerOperation.Arm(alarm)
                }
            }
        }
        return operations
    }
}
