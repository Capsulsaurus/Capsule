package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.AlbumMember
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull
import java.time.Instant

/** Why a membership change was refused, in terms the screen can explain. */
sealed interface MembershipRefusal {
    /** The viewer does not hold the album's admin-tier key, so MLS would refuse the commit. */
    data object NotAdmin : MembershipRefusal

    /** A view album has no membership at all — it holds no key and is no sharing boundary. */
    data object NotAContainer : MembershipRefusal

    /** The named account is already a member; changing their role is the operation wanted. */
    data object AlreadyMember : MembershipRefusal

    /** The invite field was empty or whitespace. */
    data object EmptyRecipient : MembershipRefusal

    /** Removing the last admin would leave the album with nobody able to administer it. */
    data object LastAdmin : MembershipRefusal
}

/** Either the album as it should now be, or the reason nothing changed. */
sealed interface MembershipEdit {
    /** The edit succeeded; [album] is what to write back. */
    data class Applied(
        val album: Album,
    ) : MembershipEdit

    /** Nothing changed. */
    data class Refused(
        val reason: MembershipRefusal,
    ) : MembershipEdit
}

/**
 * The rules an album's membership list obeys, as pure functions over the album.
 *
 * Kept out of the view model so they can be exercised without a coroutine or a repository,
 * and kept out of the repository because they are not storage rules: every one of them is
 * really a statement about what MLS would accept. A client that let the user *attempt* an
 * invite it has no admin key for would be a client that reports a failure the server has to
 * explain; refusing locally, with a reason, is both faster and honest.
 *
 * None of this is access control. A capability *is* which keys MLS delivered, so these
 * functions cannot grant anything — they only keep the client from proposing a commit that
 * cannot succeed.
 */
object AlbumMembership {
    /** Whether the viewer holds the admin tier on [album]. */
    fun viewerIsAdmin(album: Album): Boolean = album.viewerCapability?.knownOrNull == AlbumCapability.Admin

    /** Adds [recipient] at [capability], as the album would look after the MLS `Add`. */
    fun invite(
        album: Album,
        recipient: String,
        capability: AlbumCapability,
        joinedAt: Instant,
    ): MembershipEdit {
        val handle = recipient.trim()
        val refusal =
            when {
                !album.isContainer() -> MembershipRefusal.NotAContainer
                !viewerIsAdmin(album) -> MembershipRefusal.NotAdmin
                handle.isEmpty() -> MembershipRefusal.EmptyRecipient
                album.members.any { it.userId.value == handle } -> MembershipRefusal.AlreadyMember
                else -> null
            }
        if (refusal != null) return MembershipEdit.Refused(refusal)
        val member =
            AlbumMember(userId = UserId(handle), capability = capability.asWire(), joinedAt = joinedAt)
        return MembershipEdit.Applied(album.copy(members = album.members + member))
    }

    /** Removes [userId] — an MLS `Remove` plus an epoch bump, once it reaches the group. */
    fun remove(
        album: Album,
        userId: UserId,
    ): MembershipEdit {
        val refusal =
            when {
                !album.isContainer() -> MembershipRefusal.NotAContainer
                !viewerIsAdmin(album) -> MembershipRefusal.NotAdmin
                wouldStripLastAdmin(album, userId) -> MembershipRefusal.LastAdmin
                else -> null
            }
        if (refusal != null) return MembershipEdit.Refused(refusal)
        return MembershipEdit.Applied(album.copy(members = album.members.filterNot { it.userId == userId }))
    }

    /** Moves [userId] to [capability] — also an MLS commit, and also an epoch bump. */
    fun setCapability(
        album: Album,
        userId: UserId,
        capability: AlbumCapability,
    ): MembershipEdit {
        val demotesLastAdmin = capability != AlbumCapability.Admin && wouldStripLastAdmin(album, userId)
        val refusal =
            when {
                !album.isContainer() -> MembershipRefusal.NotAContainer
                !viewerIsAdmin(album) -> MembershipRefusal.NotAdmin
                demotesLastAdmin -> MembershipRefusal.LastAdmin
                else -> null
            }
        if (refusal != null) return MembershipEdit.Refused(refusal)
        val members =
            album.members.map { member ->
                if (member.userId == userId) member.copy(capability = capability.asWire()) else member
            }
        return MembershipEdit.Applied(album.copy(members = members))
    }

    /** Whether removing or demoting [userId] would leave the album with no admin at all. */
    private fun wouldStripLastAdmin(
        album: Album,
        userId: UserId,
    ): Boolean {
        val admins = album.members.filter { it.capability.knownOrNull == AlbumCapability.Admin }
        return admins.size == 1 && admins.single().userId == userId
    }

    private fun Album.isContainer(): Boolean = kind.knownOrNull == AlbumKind.Container
}
