package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * Reporting, blocking, and contesting a decision.
 *
 * Filled in by `:capsule-android:feature:moderation`.
 */
fun EntryProviderScope<NavKey>.moderationEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.ModerationReport>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.ModerationBlocks>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.ModerationAppeal>(scope) { scope.Placeholder(it) }
}
