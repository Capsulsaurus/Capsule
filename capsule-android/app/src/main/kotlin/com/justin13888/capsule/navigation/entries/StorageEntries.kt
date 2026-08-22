package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * What the library occupies and what the account is allowed.
 *
 * Filled in by `:capsule-android:feature:storage`.
 */
fun EntryProviderScope<NavKey>.storageEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Storage>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Quota>(scope) { scope.Placeholder(it) }
}
