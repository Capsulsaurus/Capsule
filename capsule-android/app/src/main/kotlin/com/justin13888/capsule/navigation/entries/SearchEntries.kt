package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * Search.
 *
 * Only one destination: the saved searches it produces are smart albums, and those belong to
 * the albums area because that is where the user finds them again.
 *
 * Filled in by `:capsule-android:feature:search`.
 */
fun EntryProviderScope<NavKey>.searchEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Search>(scope) { scope.Placeholder(it) }
}
