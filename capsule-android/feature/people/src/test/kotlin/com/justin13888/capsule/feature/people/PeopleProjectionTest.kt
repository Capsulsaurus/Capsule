package com.justin13888.capsule.feature.people

import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.FaceClusterId
import com.justin13888.capsule.core.model.ModelOutputStatus
import com.justin13888.capsule.core.model.asWire
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The projection the People screens draw from.
 *
 * The exclusions are what matter: a grouping from a replaced model version must be reported
 * rather than silently dropped, and a suggestion from one must never be offered for promotion
 * — writing a value the library has already stopped trusting into the user's own namespace is
 * the one direction the AI/user separation exists to prevent.
 */
class PeopleProjectionTest {
    @Test
    fun `a superseded grouping is counted and its slot named`() {
        val notice =
            requireNotNull(
                PeopleProjection.staleNotice(
                    listOf(
                        cluster("fresh", ModelOutputStatus.Current),
                        cluster("stale", ModelOutputStatus.Stale),
                    ),
                ),
            )

        assertEquals(1, notice.staleCount)
        assertEquals(listOf("facenet-s 2025.05"), notice.slots)
    }

    @Test
    fun `a library on the canonical slot raises no notice`() {
        assertNull(PeopleProjection.staleNotice(listOf(cluster("fresh", ModelOutputStatus.Current))))
    }

    @Test
    fun `only current suggestions are offered for promotion`() {
        val asset = AssetId("a")
        val tags = AssetTags(ai = freshAiTags("beach").ai + staleAiTags("mountain").ai)

        val offered = PeopleProjection.suggestions(listOf(asset), mapOf(asset to tags), limit = 10)

        assertEquals(listOf("beach"), offered.map(AiSuggestion::tag))
    }

    @Test
    fun `a suggestion carries the add it targets, not just the word`() {
        val asset = AssetId("a")

        val offered = PeopleProjection.suggestions(listOf(asset), mapOf(asset to freshAiTags("beach")), limit = 10)

        assertEquals("device-1", offered.single().deviceId)
        assertEquals(0L, offered.single().counter)
    }

    @Test
    fun `the same word suggested on two photographs is offered once`() {
        val first = AssetId("a")
        val second = AssetId("b")
        val tags = mapOf(first to freshAiTags("beach"), second to freshAiTags("beach"))

        val offered = PeopleProjection.suggestions(listOf(first, second), tags, limit = 10)

        assertEquals(1, offered.size)
    }

    @Test
    fun `the offer is bounded so the strip stays readable`() {
        val asset = AssetId("a")
        val tags = mapOf(asset to freshAiTags("a", "b", "c", "d", "e"))

        assertEquals(2, PeopleProjection.suggestions(listOf(asset), tags, limit = 2).size)
    }

    @Test
    fun `every failure resolves to something the screen can say`() {
        assertEquals(PeopleFailure.Offline, PeopleProjection.failureOf(LocalError.Offline))
        assertEquals(PeopleFailure.Missing, PeopleProjection.failureOf(LocalError.NotFound("person")))
        assertEquals(PeopleFailure.Suspended, PeopleProjection.failureOf(ModerationError.AccountSuspended))
        assertEquals(PeopleFailure.QuotaLocked, PeopleProjection.failureOf(QuotaError.GraceLocked))
        assertEquals(PeopleFailure.Unsupported, PeopleProjection.failureOf(ProtocolError.VersionUnsupported))
        assertTrue(PeopleProjection.failureOf(LocalError.Cancelled) == PeopleFailure.Unknown)
    }

    private fun cluster(
        id: String,
        status: ModelOutputStatus,
    ) = FaceCluster(
        id = FaceClusterId(id),
        slot = if (status == ModelOutputStatus.Current) PeopleFixtures.CURRENT_SLOT else PeopleFixtures.SUPERSEDED_SLOT,
        status = status.asWire(),
        assetIds = listOf(AssetId("x")),
    )
}
