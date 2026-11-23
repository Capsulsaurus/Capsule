package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.AlbumId
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * The tiny grammar the album destination's id carries.
 *
 * Every case here is really the same property — a ref survives a trip through the nav key
 * unchanged — but the round trip is only half of it. The other half is that parsing is
 * **total**: an id this build does not recognise must still resolve to something the entry
 * provider can render, because a throw inside the entry provider takes the whole back stack
 * with it.
 */
class CollectionRefTest {
    @Test
    fun `a container id travels verbatim`() {
        val ref = CollectionRef.Container(AlbumId("018f-abc"))
        assertEquals("018f-abc", ref.encode())
        assertEquals(ref, CollectionRef.parse(ref.encode()))
    }

    @Test
    fun `every system view round trips`() {
        SystemViewKind.entries.forEach { kind ->
            val ref = CollectionRef.SystemView(kind)
            assertEquals(ref, CollectionRef.parse(ref.encode()))
        }
    }

    @Test
    fun `the stacks listing round trips`() {
        assertEquals(CollectionRef.Stacks, CollectionRef.parse(CollectionRef.Stacks.encode()))
    }

    @Test
    fun `an aggregate round trips`() {
        val ref = CollectionRef.Federated("018f-group")
        assertEquals("group:018f-group", ref.encode())
        assertEquals(ref, CollectionRef.parse(ref.encode()))
    }

    @Test
    fun `an unrecognised view token falls back to a container rather than throwing`() {
        val parsed = CollectionRef.parse("view:from-a-newer-build")
        assertEquals(CollectionRef.Container(AlbumId("view:from-a-newer-build")), parsed)
    }
}
