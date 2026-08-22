package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * People and pets recognised across the library.
 *
 * Filled in by `:capsule-android:feature:people`.
 */
fun EntryProviderScope<NavKey>.peopleEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.People>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.PersonDetail>(scope) { scope.Placeholder(it) }
}
