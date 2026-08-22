package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * Settings and its sub-pages.
 *
 * Sub-pages are real destinations rather than expanding sections so that a link from
 * elsewhere in the app — "change this in Sync settings" — has somewhere to point.
 *
 * Filled in by `:capsule-android:feature:settings`.
 */
fun EntryProviderScope<NavKey>.settingsEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Settings>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.SettingsAppearance>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.SettingsLibrary>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.SettingsSync>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.SettingsPrivacy>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.SettingsAbout>(scope) { scope.Placeholder(it) }
}
