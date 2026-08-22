package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * Everything that decides whether a lost device is an inconvenience or a lost library.
 *
 * Six destinations for what could be a wizard, because each is somewhere a user needs to be
 * able to arrive at directly — from a reminder, from a support article, from settings — and a
 * wizard step with no key of its own cannot be linked to.
 *
 * Filled in by `:capsule-android:feature:recovery`.
 */
fun EntryProviderScope<NavKey>.recoveryEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.RecoveryStatus>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.RecoveryVerify>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.RecoveryRewrap>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Shamir>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.BackupExport>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.BackupRestore>(scope) { scope.Placeholder(it) }
}
