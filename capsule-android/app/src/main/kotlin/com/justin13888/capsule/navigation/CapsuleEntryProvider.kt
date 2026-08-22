package com.justin13888.capsule.navigation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavEntry
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import com.justin13888.capsule.navigation.entries.albumsEntries
import com.justin13888.capsule.navigation.entries.auditEntries
import com.justin13888.capsule.navigation.entries.dropsEntries
import com.justin13888.capsule.navigation.entries.enrollmentEntries
import com.justin13888.capsule.navigation.entries.importerEntries
import com.justin13888.capsule.navigation.entries.moderationEntries
import com.justin13888.capsule.navigation.entries.onboardingEntries
import com.justin13888.capsule.navigation.entries.peopleEntries
import com.justin13888.capsule.navigation.entries.recoveryEntries
import com.justin13888.capsule.navigation.entries.searchEntries
import com.justin13888.capsule.navigation.entries.securityEntries
import com.justin13888.capsule.navigation.entries.settingsEntries
import com.justin13888.capsule.navigation.entries.sharingEntries
import com.justin13888.capsule.navigation.entries.storageEntries
import com.justin13888.capsule.navigation.entries.syncEntries
import com.justin13888.capsule.navigation.entries.timelineEntries
import com.justin13888.capsule.navigation.entries.viewerEntries
import com.justin13888.capsule.placeholder.PlaceholderScreen
import com.justin13888.capsule.shell.chrome.BiometricGate

/**
 * Declares one destination, with the gate applied wherever the key asks for one.
 *
 * Every area file goes through this rather than calling `entry` directly, so the SR1 gate is
 * enforced by the routing table and not by each area remembering to wrap. Trash and Hidden
 * are gated because [CapsuleDestination.gatedView] says so; nothing else can accidentally
 * skip it, and nothing else can accidentally acquire it.
 */
inline fun <reified K : CapsuleDestination> EntryProviderScope<NavKey>.destination(
    scope: CapsuleEntryScope,
    noinline content: @Composable (K) -> Unit,
) {
    entry<K> { key ->
        val gated = key.gatedView
        if (gated == null) {
            content(key)
        } else {
            BiometricGate(view = gated, gate = scope.gate, prompt = scope.prompt) { content(key) }
        }
    }
}

/**
 * The stand-in every destination renders until its feature module exists.
 *
 * A named helper rather than an inline call so that filling a destination in is a one-line
 * diff at the call site, and so that a search for this symbol lists exactly what is still
 * outstanding.
 */
@Composable
fun CapsuleEntryScope.Placeholder(destination: CapsuleDestination) {
    PlaceholderScreen(destination = destination, navigator = navigator)
}

/**
 * Assembles the app's whole routing table from one contribution per product area.
 *
 * Split by area on purpose: seventeen feature modules land in seventeen separate slices, and
 * a single 60-branch `entryProvider` block would make every one of them a conflict against
 * the same lines. One file per area means a slice touches one file.
 */
@Composable
fun rememberCapsuleEntryProvider(scope: CapsuleEntryScope): (NavKey) -> NavEntry<NavKey> =
    remember(scope) {
        entryProvider<NavKey> {
            timelineEntries(scope)
            albumsEntries(scope)
            peopleEntries(scope)
            searchEntries(scope)
            viewerEntries(scope)
            importerEntries(scope)
            sharingEntries(scope)
            dropsEntries(scope)
            syncEntries(scope)
            storageEntries(scope)
            securityEntries(scope)
            recoveryEntries(scope)
            moderationEntries(scope)
            auditEntries(scope)
            settingsEntries(scope)
            onboardingEntries(scope)
            enrollmentEntries(scope)
        }
    }
