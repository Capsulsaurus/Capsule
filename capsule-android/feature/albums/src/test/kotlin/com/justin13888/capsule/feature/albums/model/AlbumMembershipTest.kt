package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.AlbumMember
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.core.model.asWire
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Test
import java.time.Instant

/**
 * What a client may propose to an album's MLS group, and what it refuses to propose.
 *
 * None of this grants anything — a capability *is* which keys were delivered — so every case
 * below is about not sending a commit that cannot succeed. Refusing locally, with a reason,
 * is both faster than a round trip and the only way the user learns *why*.
 */
class AlbumMembershipTest {
    private val now: Instant = Instant.parse("2026-06-01T00:00:00Z")
    private val owner = UserId("owner")

    private fun album(
        viewer: AlbumCapability? = AlbumCapability.Admin,
        kind: AlbumKind = AlbumKind.Container,
        members: List<AlbumMember> = listOf(AlbumMember(owner, AlbumCapability.Admin.asWire(), now)),
    ) = Album(
        id = AlbumId("album"),
        name = "Family",
        createdAt = now,
        modifiedAt = now,
        kind = kind.asWire(),
        viewerCapability = viewer?.asWire(),
        members = members,
    )

    @Test
    fun `an admin can invite`() {
        val edit = AlbumMembership.invite(album(), "friend", AlbumCapability.Write, now)
        val applied = assertInstanceOf(MembershipEdit.Applied::class.java, edit)
        assertEquals(2, applied.album.members.size)
        assertEquals(
            AlbumCapability.Write.asWire(),
            applied.album.members
                .last()
                .capability,
        )
    }

    @Test
    fun `a write member cannot invite`() {
        val edit = AlbumMembership.invite(album(viewer = AlbumCapability.Write), "friend", AlbumCapability.Read, now)
        assertEquals(MembershipEdit.Refused(MembershipRefusal.NotAdmin), edit)
    }

    @Test
    fun `a view album has no membership at all`() {
        val edit = AlbumMembership.invite(album(kind = AlbumKind.View), "friend", AlbumCapability.Read, now)
        assertEquals(MembershipEdit.Refused(MembershipRefusal.NotAContainer), edit)
    }

    @Test
    fun `an empty recipient is refused before anything is proposed`() {
        val edit = AlbumMembership.invite(album(), "   ", AlbumCapability.Read, now)
        assertEquals(MembershipEdit.Refused(MembershipRefusal.EmptyRecipient), edit)
    }

    @Test
    fun `inviting an existing member is refused, because the operation wanted is a role change`() {
        val edit = AlbumMembership.invite(album(), owner.value, AlbumCapability.Read, now)
        assertEquals(MembershipEdit.Refused(MembershipRefusal.AlreadyMember), edit)
    }

    @Test
    fun `removing the last admin is refused`() {
        assertEquals(MembershipEdit.Refused(MembershipRefusal.LastAdmin), AlbumMembership.remove(album(), owner))
    }

    @Test
    fun `demoting the last admin is refused for the same reason`() {
        val edit = AlbumMembership.setCapability(album(), owner, AlbumCapability.Read)
        assertEquals(MembershipEdit.Refused(MembershipRefusal.LastAdmin), edit)
    }

    @Test
    fun `a second admin makes the first removable`() {
        val two =
            album(
                members =
                    listOf(
                        AlbumMember(owner, AlbumCapability.Admin.asWire(), now),
                        AlbumMember(UserId("other"), AlbumCapability.Admin.asWire(), now),
                    ),
            )
        val applied = assertInstanceOf(MembershipEdit.Applied::class.java, AlbumMembership.remove(two, owner))
        assertEquals(listOf(UserId("other")), applied.album.members.map(AlbumMember::userId))
    }
}
