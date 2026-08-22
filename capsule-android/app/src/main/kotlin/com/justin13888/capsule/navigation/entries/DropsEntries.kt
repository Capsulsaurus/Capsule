package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * What arrived through an upload link and has not been accepted yet.
 *
 * Filled in by `:capsule-android:feature:drops`.
 */
fun EntryProviderScope<NavKey>.dropsEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.DropInbox>(scope) { scope.Placeholder(it) }
}
