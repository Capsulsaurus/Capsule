package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.albums.AlbumRoute
import com.justin13888.capsule.feature.albums.CollectionsHubRoute
import com.justin13888.capsule.feature.albums.DuplicatesRoute
import com.justin13888.capsule.feature.albums.PlacesRoute
import com.justin13888.capsule.feature.albums.SmartAlbumBuilderRoute
import com.justin13888.capsule.feature.albums.SmartAlbumsRoute
import com.justin13888.capsule.feature.albums.StackDetailRoute
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * Albums, smart albums, stacks, and the utility views the Collections tab gathers.
 *
 * Filled in by `:capsule-android:feature:albums`. The hub itself is a feature screen rather
 * than shell furniture: the sections it lists are containers, computed views and utilities,
 * and telling those apart on screen is the feature's job — the shell only knows that the
 * Collections destination renders something.
 *
 * Trash and Hidden are deliberately absent from this file. Both carry a `gatedView`, and the
 * gate is applied by the entry provider from the destination's own flag; their entries are
 * declared alongside the timeline, which is where the shell put them. Reaching them from the
 * hub goes through the navigator, so they are always *pushed* — opening one inside a pane
 * would render it without passing through its entry, and so without the SR1 gate.
 */
fun EntryProviderScope<NavKey>.albumsEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Collections>(scope) {
        CollectionsHubRoute(
            onOpenCollection = { ref -> scope.navigator.goTo(CapsuleDestination.AlbumDetail(ref)) },
            onOpenSmartAlbums = { scope.navigator.goTo(CapsuleDestination.SmartAlbums) },
            onOpenDuplicates = { scope.navigator.goTo(CapsuleDestination.Duplicates) },
            onOpenPlaces = { scope.navigator.goTo(CapsuleDestination.Places) },
            onOpenTrash = { scope.navigator.goTo(CapsuleDestination.Trash) },
            onOpenHidden = { scope.navigator.goTo(CapsuleDestination.Hidden) },
        )
    }
    destination<CapsuleDestination.AlbumDetail>(scope) { key ->
        AlbumRoute(
            albumId = key.albumId,
            onBack = { scope.navigator.goBack() },
            onOpenAsset = { assetId ->
                scope.navigator.goTo(CapsuleDestination.AssetViewer(assetId = assetId, contextId = key.albumId))
            },
            onOpenStack = { stackId -> scope.navigator.goTo(CapsuleDestination.StackDetail(stackId)) },
        )
    }
    destination<CapsuleDestination.StackDetail>(scope) { key ->
        StackDetailRoute(stackId = key.stackId, onBack = { scope.navigator.goBack() })
    }
    destination<CapsuleDestination.SmartAlbums>(scope) {
        SmartAlbumsRoute(
            onBack = { scope.navigator.goBack() },
            onEditRule = { id -> scope.navigator.goTo(CapsuleDestination.SmartAlbumBuilder(id)) },
        )
    }
    destination<CapsuleDestination.SmartAlbumBuilder>(scope) { key ->
        SmartAlbumBuilderRoute(smartAlbumId = key.smartAlbumId, onBack = { scope.navigator.goBack() })
    }
    destination<CapsuleDestination.Places>(scope) {
        PlacesRoute(
            onBack = { scope.navigator.goBack() },
            onOpenAsset = { assetId -> scope.navigator.goTo(CapsuleDestination.AssetViewer(assetId)) },
        )
    }
    destination<CapsuleDestination.Duplicates>(scope) {
        DuplicatesRoute(onBack = { scope.navigator.goBack() })
    }
}
