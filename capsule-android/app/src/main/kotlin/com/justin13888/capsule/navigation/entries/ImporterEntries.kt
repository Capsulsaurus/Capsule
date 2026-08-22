package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * Choosing a source, reviewing the plan, and watching the run.
 *
 * Three destinations rather than one wizard because an import survives the app being closed:
 * the user must be able to come back to a running import from anywhere, which needs a key
 * that names it.
 *
 * Filled in by `:capsule-android:feature:importer`.
 */
fun EntryProviderScope<NavKey>.importerEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.ImportSources>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.ImportPlan>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.ImportProgress>(scope) { scope.Placeholder(it) }
}
