package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * Links out of the library and links into it.
 *
 * Share links and upload links are separate hierarchies on purpose. They look symmetrical but
 * carry opposite risk: a share link exposes what is already here, an upload link accepts
 * bytes from someone who cannot see anything. Collapsing them into one list would invite a
 * user to reason about them the same way.
 *
 * Filled in by `:capsule-android:feature:sharing`.
 */
fun EntryProviderScope<NavKey>.sharingEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Sharing>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.ShareLinks>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.ShareLinkCreate>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.ShareLinkDetail>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.UploadLinks>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.UploadLinkCreate>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.UploadLinkDetail>(scope) { scope.Placeholder(it) }
}
