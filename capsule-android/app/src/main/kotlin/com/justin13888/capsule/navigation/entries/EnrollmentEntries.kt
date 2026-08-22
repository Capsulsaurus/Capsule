package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * Adding a device to an account that already exists.
 *
 * Both sides of the exchange live here — the device being added and the device approving it —
 * because they are the same protocol seen from two ends, and a user runs both within a minute
 * of each other.
 *
 * Filled in by `:capsule-android:feature:enrollment`.
 */
fun EntryProviderScope<NavKey>.enrollmentEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.EnrollmentStart>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.EnrollmentScan>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.EnrollmentCode>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.EnrollmentApprove>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.EnrollmentComplete>(scope) { scope.Placeholder(it) }
}
