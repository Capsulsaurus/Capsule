package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.search.SearchRoute
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * Search.
 *
 * Only one destination: the saved searches it produces are smart albums, and those belong to
 * the albums area because that is where the user finds them again.
 *
 * The viewer is opened with the result's own id and no context: paging through a search
 * answer is a separate decision from paging through an album, and handing the viewer a
 * context it cannot re-evaluate would page the user through the wrong listing.
 */
fun EntryProviderScope<NavKey>.searchEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Search>(scope) {
        SearchRoute(
            onOpenAsset = { assetId -> scope.navigator.goTo(CapsuleDestination.AssetViewer(assetId)) },
        )
    }
}
