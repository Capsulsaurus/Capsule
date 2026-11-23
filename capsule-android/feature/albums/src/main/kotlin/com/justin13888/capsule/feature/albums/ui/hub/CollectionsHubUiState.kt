package com.justin13888.capsule.feature.albums.ui.hub

import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.feature.albums.model.CollectionRef
import com.justin13888.capsule.feature.albums.model.FederatedAlbum
import com.justin13888.capsule.feature.albums.model.SystemViewKind
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf

/**
 * One row of the hub that stands for an album the user owns or is a member of.
 *
 * [isDefault] is carried rather than derived because it changes what the row is *called*:
 * the default album is nameless by design, so a client surfaces it as the library's primary
 * view rather than as a row with an empty title.
 */
data class HubAlbum(
    val id: AlbumId,
    val name: String,
    val isDefault: Boolean,
    val assetCount: Int,
    val capability: Wire<AlbumCapability>?,
) {
    /** Where tapping this row goes. */
    val ref: CollectionRef get() = CollectionRef.Container(id)
}

/** One built-in view, with however many of the sampled assets fell into it. */
data class HubMediaType(
    val kind: SystemViewKind,
    val count: Int,
    /** Whether [count] is the whole library's answer or only the sample's. */
    val isCountComplete: Boolean,
)

/**
 * What the Collections hub is showing.
 *
 * Loading and error are separate states rather than nullable content, because the hub has a
 * real error case — the sample it counts media types from is a repository read that can fail
 * — and a screen that rendered an empty hub on failure would tell the user their library was
 * empty.
 */
sealed interface CollectionsHubUiState {
    /** Reading albums and sampling the library. */
    data object Loading : CollectionsHubUiState

    /** The read failed; [code] is the stable catalogue code to localize. */
    data class Failed(
        val code: String?,
    ) : CollectionsHubUiState

    /** The four sections, in the order the hub shows them. */
    data class Content(
        /** Container albums: real MLS groups, the only things photos can be filed into. */
        val myAlbums: ImmutableList<HubAlbum> = persistentListOf(),
        /** Built-in views computed from the local index. */
        val mediaTypes: ImmutableList<HubMediaType> = persistentListOf(),
        /** View albums the account holds — key-free, owning nothing. */
        val viewAlbums: ImmutableList<HubAlbum> = persistentListOf(),
        /** Aggregated federated albums, which are views spanning several home servers. */
        val federated: ImmutableList<FederatedAlbum> = persistentListOf(),
        /** How many saved searches present themselves as albums. */
        val smartAlbumCount: Int = 0,
    ) : CollectionsHubUiState {
        /** Whether the account has nothing to show at all — the first-run hub. */
        val isEmpty: Boolean
            get() = myAlbums.isEmpty() && viewAlbums.isEmpty() && federated.isEmpty()
    }
}
