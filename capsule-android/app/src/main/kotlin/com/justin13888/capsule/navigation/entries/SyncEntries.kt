package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * Sync state, the upload queue behind it, and whatever it refused to accept.
 *
 * Filled in by `:capsule-android:feature:sync`.
 */
fun EntryProviderScope<NavKey>.syncEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.SyncCentre>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.UploadQueue>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Quarantine>(scope) { scope.Placeholder(it) }
}
