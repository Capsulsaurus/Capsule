package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * The account's security-relevant event log.
 *
 * Filled in by `:capsule-android:feature:audit`.
 */
fun EntryProviderScope<NavKey>.auditEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.AuditLog>(scope) { scope.Placeholder(it) }
}
