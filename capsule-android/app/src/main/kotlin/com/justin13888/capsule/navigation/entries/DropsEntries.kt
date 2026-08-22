package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.drops.DropInboxRoute
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * What arrived through an upload link and has not been accepted yet.
 *
 * Filled in by `:capsule-android:feature:drops`. The inbox is a review surface, so this
 * destination is reached deliberately from the sharing hub rather than being surfaced as a
 * top-level section that would imply the deposits are already part of the library.
 */
fun EntryProviderScope<NavKey>.dropsEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.DropInbox>(scope) {
        DropInboxRoute(onBack = { scope.navigator.goBack() })
    }
}
