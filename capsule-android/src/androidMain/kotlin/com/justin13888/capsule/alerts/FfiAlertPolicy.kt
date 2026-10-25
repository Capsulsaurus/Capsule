package com.justin13888.capsule.alerts

import uniffi.capsule_sdk.FfiAlertClass
import uniffi.capsule_sdk.FfiException
import uniffi.capsule_sdk.FfiNotifyInput
import uniffi.capsule_sdk.FfiQuotaAdvisory
import java.time.Instant
import uniffi.capsule_sdk.evaluateAlerts as ffiEvaluateAlerts
import uniffi.capsule_sdk.preArmDeadlines as ffiPreArmDeadlines

/**
 * [AlertPolicy] over the core's exports - `evaluate_alerts` and `pre_arm_deadlines`
 * from `capsule-sdk::ffi::notify`. Nothing here decides anything: it converts the
 * facts to the flat FFI record (instants as RFC 3339) and the answer back.
 *
 * A refused input (`FfiException`) is rethrown as [IllegalArgumentException], which the
 * coordinator treats as "leave the armed alarms as they were".
 *
 * A core that cannot be reached - no `libcapsule_sdk.so` or no JNA dispatch library
 * in the APK, which surfaces as a [LinkageError] (`UnsatisfiedLinkError` on the first
 * call, `NoClassDefFoundError` on every later one) - is rethrown as
 * [IllegalStateException], which the coordinator treats the same way. It never escapes
 * to the activity: a missing native library degrades alerts, it does not crash launch.
 */
class FfiAlertPolicy : AlertPolicy {
    override fun evaluate(
        facts: NotifyFacts,
        now: Instant,
    ): List<Alert> =
        call {
            ffiEvaluateAlerts(facts.toFfi(), now.toString()).map { alert ->
                Alert(alert.`class`.toAlertClass(), alert.deadline?.let(Instant::parse), alert.params)
            }
        }

    override fun preArmDeadlines(
        facts: NotifyFacts,
        now: Instant,
    ): List<ClassDeadline> =
        call {
            ffiPreArmDeadlines(facts.toFfi(), now.toString()).map {
                ClassDeadline(it.`class`.toAlertClass(), Instant.parse(it.deadline))
            }
        }

    private fun <T> call(body: () -> T): T =
        try {
            body()
        } catch (error: FfiException) {
            throw IllegalArgumentException("the alert predicate refused its input", error)
        } catch (error: LinkageError) {
            throw IllegalStateException("the alert predicate's native library is unavailable", error)
        }

    private fun NotifyFacts.toFfi() =
        FfiNotifyInput(
            lastCompletedSync = lastCompletedSync?.toString(),
            unsyncedChanges = unsyncedChanges.coerceAtLeast(0).toULong(),
            recoveryNextDue = recoveryNextDue?.toString(),
            recoverySnoozedUntil = recoverySnoozedUntil?.toString(),
            recoverySnoozeBudgetSpent = recoverySnoozeBudgetSpent,
            recoveryRewrapDue = recoveryRewrapDue,
            quotaState = quotaState?.toFfi(),
            quarantinePending = quarantinePending.coerceAtLeast(0).toULong(),
            dropsPending = dropsPending.coerceAtLeast(0).toULong(),
            suppressedUntil = suppressedUntil.entries.associate { (alertClass, until) -> alertClass.wire to until.toString() },
            disabled = disabled.map { it.wire },
        )

    private fun QuotaAdvisory.toFfi(): FfiQuotaAdvisory =
        when (this) {
            QuotaAdvisory.OK -> FfiQuotaAdvisory.OK
            QuotaAdvisory.SOFT_WARNING -> FfiQuotaAdvisory.SOFT_WARNING
            QuotaAdvisory.HARD_EXCEEDED -> FfiQuotaAdvisory.HARD_EXCEEDED
            QuotaAdvisory.GRACE_EXPIRED -> FfiQuotaAdvisory.GRACE_EXPIRED
            QuotaAdvisory.SUSPENDED -> FfiQuotaAdvisory.SUSPENDED
        }

    private fun FfiAlertClass.toAlertClass(): AlertClass =
        when (this) {
            FfiAlertClass.SYNC_STALE -> AlertClass.SYNC_STALE
            FfiAlertClass.RECOVERY_CHECK_DUE -> AlertClass.RECOVERY_CHECK_DUE
            FfiAlertClass.QUOTA_SOFT -> AlertClass.QUOTA_SOFT
            FfiAlertClass.QUOTA_GRACE_EXPIRING -> AlertClass.QUOTA_GRACE_EXPIRING
            FfiAlertClass.QUARANTINE_PENDING -> AlertClass.QUARANTINE_PENDING
            FfiAlertClass.DROP_PENDING -> AlertClass.DROP_PENDING
        }
}
