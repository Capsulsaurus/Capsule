package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.timeline.TimelineRoute
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
 * Filled in by `:capsule-android:feature:timeline`. The main listing is live; Recently Deleted
 * and Hidden still render the placeholder, because the retention countdown and restore flow
 * those two screens need belong to the Collections slice's specification rather than this one.
 */
fun EntryProviderScope<NavKey>.timelineEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Timeline>(scope) {
        TimelineRoute(
            onOpenAsset = { assetId -> scope.navigator.goTo(CapsuleDestination.AssetViewer(assetId)) },
            onImport = { scope.navigator.goTo(CapsuleDestination.ImportSources) },
            // Both hand-offs land on the surface that owns them. The share link is minted with
            // no album pre-scoped: the timeline's selection is a set of assets, not an album,
            // and pre-filling one would be a guess the user would then have to notice and undo.
            // Adding to an album lands on the collections hub until that slice declares a
            // picker destination — there is no key today that means "choose an album".
            onShareAssets = { scope.navigator.goTo(CapsuleDestination.ShareLinkCreate()) },
            onAddToAlbum = { scope.navigator.goTo(CapsuleDestination.Collections) },
        )
    }
    destination<CapsuleDestination.Trash>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.Hidden>(scope) { scope.Placeholder(it) }
}
