package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * The library listing and the two views that sit beneath it.
 *
 * Trash and Hidden are declared here rather than under Collections because they are the same
 * listing with a different filter — the timeline feature owns the listing, so it owns them.
 * Both carry a gated view on their key, so `destination` wraps them in the local-auth gate
 * without this file mentioning it.
 *
 * Filled in by `:capsule-android:feature:timeline`.
 */
fun EntryProviderScope<NavKey>.timelineEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Timeline>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Trash>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Hidden>(scope) { scope.Placeholder(it) }
}
