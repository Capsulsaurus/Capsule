package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.people.PeopleRoute
import com.justin13888.capsule.feature.people.PersonDetailRoute
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * People and pets recognised across the library.
 *
 * A photograph opened from a person carries that person as the viewer's context, so paging
 * stays inside the listing the user was looking at rather than escaping into the whole
 * library; one opened from the hub's smart albums carries none, because a computed album is
 * re-derived rather than addressable.
 */
fun EntryProviderScope<NavKey>.peopleEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.People>(scope) {
        PeopleRoute(
            onOpenPerson = { personId -> scope.navigator.goTo(CapsuleDestination.PersonDetail(personId)) },
            onOpenAsset = { assetId -> scope.navigator.goTo(CapsuleDestination.AssetViewer(assetId)) },
        )
    }
    destination<CapsuleDestination.PersonDetail>(scope) { key ->
        PersonDetailRoute(
            personId = key.personId,
            onBack = { scope.navigator.goBack() },
            onOpenAsset = { assetId ->
                scope.navigator.goTo(CapsuleDestination.AssetViewer(assetId, contextId = key.personId))
            },
        )
    }
}
