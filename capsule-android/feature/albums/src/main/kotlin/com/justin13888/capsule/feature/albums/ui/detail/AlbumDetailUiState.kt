package com.justin13888.capsule.feature.albums.ui.detail

import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.feature.albums.model.AlbumPolicy
import com.justin13888.capsule.feature.albums.model.FederatedAlbum
import com.justin13888.capsule.feature.albums.model.MembershipRefusal
import com.justin13888.capsule.feature.albums.model.SystemViewKind
import com.justin13888.capsule.feature.albums.model.UpgradePhase
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf

/**
 * Which of the three things this screen is showing.
 *
 * Kept explicit rather than inferred from which fields happen to be non-null, because the
 * affordances differ in ways that matter: only a container can be renamed, shared or made the
 * default, and only an aggregate has to say out loud that you can remove only your own
 * photos.
 */
sealed interface AlbumDetailKind {
    /** A real container album — an MLS group, a sharing boundary, an import destination. */
    data object Container : AlbumDetailKind

    /** A built-in view computed from the local index; holds no keys, owns nothing. */
    data class View(
        val kind: SystemViewKind,
    ) : AlbumDetailKind

    /** An aggregated federated album: several contributors' own albums, shown together. */
    data object Aggregate : AlbumDetailKind
}

/** Something the screen has to say once, in response to what the user just did. */
sealed interface AlbumDetailMessage {
    /** A membership edit was refused before it was ever proposed to the group. */
    data class Refused(
        val reason: MembershipRefusal,
    ) : AlbumDetailMessage

    /** A repository call failed; [code] is the stable catalogue code. */
    data class Failed(
        val code: String?,
    ) : AlbumDetailMessage

    /** The album is the designated default, so it cannot be deleted while it is. */
    data object DefaultUndeletable : AlbumDetailMessage
}

/** What album detail is showing. */
sealed interface AlbumDetailUiState {
    /** Resolving the reference and reading the first window of contents. */
    data object Loading : AlbumDetailUiState

    /** The album could not be read; [code] is the stable catalogue code to localize. */
    data class Failed(
        val code: String?,
    ) : AlbumDetailUiState

    /** The album, its contents, and everything the sheets need. */
    data class Content(
        /** What to put in the app bar. Already resolved, including the default album's name. */
        val title: String,
        /** Which of the three shapes this is. */
        val kind: AlbumDetailKind,
        /** The window of contents currently loaded. */
        val assets: ImmutableList<Asset> = persistentListOf(),
        /** Whether more contents exist than were loaded. */
        val isTruncated: Boolean = false,
        /** The container itself, for the membership sheet. `null` for a view. */
        val album: Album? = null,
        /** The container's fixed policy. `null` for a view. */
        val policy: AlbumPolicy? = null,
        /** Where an upgrade ceremony has got to. */
        val upgrade: UpgradePhase = UpgradePhase.Idle,
        /** The aggregate this is a rendering of, when it is one. */
        val federated: FederatedAlbum? = null,
        /** Whether the viewer holds the admin tier, and so may edit membership. */
        val isAdmin: Boolean = false,
        /** Whether this container is the owner's designated default. */
        val isDefault: Boolean = false,
        /** A one-off message about the last action. */
        val message: AlbumDetailMessage? = null,
    ) : AlbumDetailUiState
}
