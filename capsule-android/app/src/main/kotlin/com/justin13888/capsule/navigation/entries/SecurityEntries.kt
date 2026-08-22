package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * The security hub and the two lists it fronts.
 *
 * Filled in by `:capsule-android:feature:security`.
 */
fun EntryProviderScope<NavKey>.securityEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.SecurityHub>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.SessionsAndDevices>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Passkeys>(scope) { scope.Placeholder(it) }
}
