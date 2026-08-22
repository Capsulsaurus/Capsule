package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.drops.UploadLinkCreateRoute
import com.justin13888.capsule.feature.drops.UploadLinkDetailRoute
import com.justin13888.capsule.feature.drops.UploadLinksRoute
import com.justin13888.capsule.feature.sharing.ShareLinkCreateRoute
import com.justin13888.capsule.feature.sharing.ShareLinkDetailRoute
import com.justin13888.capsule.feature.sharing.ShareLinksRoute
import com.justin13888.capsule.feature.sharing.SharingHubRoute
import com.justin13888.capsule.feature.sharing.ui.SharingHubActions
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * Links out of the library and links into it.
 *
 * Share links and upload links are separate hierarchies on purpose. They look symmetrical but
 * carry opposite risk: a share link exposes what is already here, an upload link accepts
 * bytes from someone who cannot see anything. Collapsing them into one list would invite a
 * user to reason about them the same way.
 *
 * The outbound half is filled by `:capsule-android:feature:sharing` and the inbound half by
 * `:capsule-android:feature:drops`, which owns upload links because it owns what arrives
 * through them.
 */
fun EntryProviderScope<NavKey>.sharingEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Sharing>(scope) {
        SharingHubRoute(
            actions =
                SharingHubActions(
                    openShareLinks = { scope.navigator.goTo(CapsuleDestination.ShareLinks) },
                    openUploadLinks = { scope.navigator.goTo(CapsuleDestination.UploadLinks) },
                    openDropInbox = { scope.navigator.goTo(CapsuleDestination.DropInbox) },
                    createShareLink = { scope.navigator.goTo(CapsuleDestination.ShareLinkCreate()) },
                    createUploadLink = { scope.navigator.goTo(CapsuleDestination.UploadLinkCreate) },
                    // Wired by the route to its own view model; the shell never retries for it.
                    retry = {},
                ),
        )
    }
    destination<CapsuleDestination.ShareLinks>(scope) {
        ShareLinksRoute(
            onOpenLink = { shareId -> scope.navigator.goTo(CapsuleDestination.ShareLinkDetail(shareId)) },
            onCreate = { scope.navigator.goTo(CapsuleDestination.ShareLinkCreate()) },
            onBack = { scope.navigator.goBack() },
        )
    }
    destination<CapsuleDestination.ShareLinkCreate>(scope) { key ->
        ShareLinkCreateRoute(
            // Straight to the new link: its fragment secret exists exactly once, and this is
            // the only moment anything can show or copy it.
            onCreated = { shareId -> scope.navigator.goTo(CapsuleDestination.ShareLinkDetail(shareId)) },
            onBack = { scope.navigator.goBack() },
            albumId = key.albumId,
        )
    }
    destination<CapsuleDestination.ShareLinkDetail>(scope) { key ->
        ShareLinkDetailRoute(shareId = key.shareId, onBack = { scope.navigator.goBack() })
    }
    destination<CapsuleDestination.UploadLinks>(scope) {
        UploadLinksRoute(
            onOpenLink = { linkId -> scope.navigator.goTo(CapsuleDestination.UploadLinkDetail(linkId)) },
            onCreate = { scope.navigator.goTo(CapsuleDestination.UploadLinkCreate) },
            onBack = { scope.navigator.goBack() },
        )
    }
    destination<CapsuleDestination.UploadLinkCreate>(scope) {
        UploadLinkCreateRoute(
            onCreated = { linkId -> scope.navigator.goTo(CapsuleDestination.UploadLinkDetail(linkId)) },
            onBack = { scope.navigator.goBack() },
        )
    }
    destination<CapsuleDestination.UploadLinkDetail>(scope) { key ->
        UploadLinkDetailRoute(uploadLinkId = key.uploadLinkId, onBack = { scope.navigator.goBack() })
    }
}
