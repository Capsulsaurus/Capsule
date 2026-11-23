package com.justin13888.capsule.feature.people

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Test
import java.util.UUID

/**
 * The one identifier this client mints for itself.
 *
 * `PeopleRepository` has no create call — a person's id has to exist before `assignCluster`
 * can name it — so the shape is worth pinning down rather than assuming: the wrong version
 * nibble would be accepted here and rejected by whatever eventually validates it.
 */
class PersonIdsTest {
    @Test
    fun `it mints a version 7 uuid`() {
        val minted = UUID.fromString(mintPersonId().value)

        assertEquals(7, minted.version())
        assertEquals(2, minted.variant())
    }

    @Test
    fun `the timestamp prefix orders two ids minted a second apart`() {
        val earlier = mintPersonId(nowMillis = 1_700_000_000_000)
        val later = mintPersonId(nowMillis = 1_700_000_001_000)

        assertEquals(-1, earlier.value.compareTo(later.value).coerceIn(-1, 1))
    }

    @Test
    fun `two ids minted in the same millisecond still differ`() {
        assertNotEquals(mintPersonId(nowMillis = 1_700_000_000_000), mintPersonId(nowMillis = 1_700_000_000_000))
    }
}
