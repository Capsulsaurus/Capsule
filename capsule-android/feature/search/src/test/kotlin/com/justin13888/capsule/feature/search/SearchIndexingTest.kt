package com.justin13888.capsule.feature.search

import com.justin13888.capsule.core.model.AddId
import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.FaceClusterId
import com.justin13888.capsule.core.model.ModelId
import com.justin13888.capsule.core.model.ModelOutputStatus
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.ModelVersion
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.PersonId
import com.justin13888.capsule.core.model.UserTag
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.asWire
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Duration
import java.time.Instant

/**
 * The projection from catalog records into the facet space.
 *
 * The cases that matter here are the exclusions. Search must not see an AI output produced by
 * a superseded model version — the vector spaces differ across versions, so a match through
 * one would be a comparison the design forbids — and it must not silently drop a token a
 * newer peer wrote, because those assets would then be unreachable from this screen.
 */
class SearchIndexingTest {
    private val currentSlot = ModelSlot(ModelId("facenet-s"), ModelVersion("2026.02"))
    private val supersededSlot = ModelSlot(ModelId("facenet-s"), ModelVersion("2025.05"))
    private val device = DeviceId("device-1")

    @Test
    fun `an asset lands on one point of every dimension it can answer`() {
        val points =
            SearchIndexing.pointsOf(
                asset = testAsset("a", rating = 4, cull = CullFlag.Pick.asWire(), gps = testFix()),
                tags = AssetTags(user = listOf(UserTag("keeper", AddId(device, 1)))),
                people = emptyList(),
                album = null,
            )

        val kinds = points.map { it.ref.kind }.toSet()
        assertTrue(FacetKind.Date in kinds)
        assertTrue(FacetKind.MediaKind in kinds)
        assertTrue(FacetKind.Rating in kinds)
        assertTrue(FacetKind.Cull in kinds)
        assertTrue(FacetKind.Tag in kinds)
        assertTrue(FacetKind.Place in kinds)
        assertTrue(FacetKind.Camera in kinds)
        assertTrue(FacetKind.Album in kinds)
    }

    @Test
    fun `a stale AI tag is not a searchable tag`() {
        val tags =
            AssetTags(
                ai =
                    listOf(
                        AiTag("beach", AddId(device, 1), currentSlot),
                        AiTag("mountain", AddId(device, 2), supersededSlot, isStale = true),
                    ),
            )

        val tagIds =
            SearchIndexing
                .pointsOf(testAsset("a"), tags, emptyList(), null)
                .filter { it.ref.kind == FacetKind.Tag }
                .map { it.ref.id }

        assertEquals(listOf("beach"), tagIds)
    }

    @Test
    fun `a user tag and an AI tag with the same text collapse to one point`() {
        val tags =
            AssetTags(
                user = listOf(UserTag("beach", AddId(device, 1))),
                ai = listOf(AiTag("beach", AddId(device, 2), currentSlot)),
            )

        val tagPoints =
            SearchIndexing
                .pointsOf(testAsset("a"), tags, emptyList(), null)
                .filter { it.ref.kind == FacetKind.Tag }

        assertEquals(1, tagPoints.size)
    }

    @Test
    fun `an unknown wire token becomes a point of its own rather than being dropped`() {
        val points =
            SearchIndexing.pointsOf(
                asset = testAsset("a", assetType = Wire.Unknown("volumetric"), cull = Wire.Unknown("maybe")),
                tags = AssetTags(),
                people = emptyList(),
                album = null,
            )

        assertEquals("volumetric", points.single { it.ref.kind == FacetKind.MediaKind }.ref.id)
        assertEquals("maybe", points.single { it.ref.kind == FacetKind.Cull }.ref.id)
    }

    @Test
    fun `a video is projected with a formatted running time`() {
        val item =
            SearchIndexing.itemOf(
                testAsset("a", assetType = AssetType.Video.asWire()).copy(duration = Duration.ofSeconds(83)),
            )

        assertEquals("1:23", item.videoDuration)
    }

    @Test
    fun `only clusters on the canonical model slot answer a people query`() {
        val fresh = cluster("fresh", ModelOutputStatus.Current, currentSlot, listOf("a"))
        val stale = cluster("stale", ModelOutputStatus.Stale, supersededSlot, listOf("b"))
        val person =
            Person(
                id = PersonId("person-1"),
                displayName = "Mira",
                clusterIds = listOf(fresh.id, stale.id),
            )

        val index = SearchIndexing.peopleIndex(listOf(person), listOf(fresh, stale))

        assertEquals(setOf("a"), index.keys.map { it.value }.toSet())
    }

    @Test
    fun `an asset in two clusters of the same person is counted once`() {
        val one = cluster("one", ModelOutputStatus.Current, currentSlot, listOf("a"))
        val two = cluster("two", ModelOutputStatus.Current, currentSlot, listOf("a"))
        val person = Person(PersonId("p"), "Mira", clusterIds = listOf(one.id, two.id))

        val index = SearchIndexing.peopleIndex(listOf(person), listOf(one, two))

        assertEquals(1, index.values.single().size)
    }

    @Test
    fun `the notice counts both halves of the exclusion and names the slots`() {
        val stale = cluster("stale", ModelOutputStatus.Stale, supersededSlot, listOf("b"))
        val tags = listOf(AssetTags(ai = listOf(AiTag("mountain", AddId(device, 1), supersededSlot, isStale = true))))

        val notice = requireNotNull(SearchIndexing.staleNotice(listOf(stale), tags))

        assertEquals(2, notice.excludedCount)
        assertEquals(listOf("facenet-s 2025.05"), notice.slots)
    }

    @Test
    fun `a library on current slots raises no notice`() {
        val fresh = cluster("fresh", ModelOutputStatus.Current, currentSlot, listOf("a"))

        assertNull(SearchIndexing.staleNotice(listOf(fresh), listOf(AssetTags())))
    }

    @Test
    fun `sections begin where the capture year changes`() {
        val items =
            listOf(
                SearchIndexing.itemOf(testAsset("a", Instant.parse("2026-01-02T00:00:00Z"))),
                SearchIndexing.itemOf(testAsset("b", Instant.parse("2026-01-01T00:00:00Z"))),
                SearchIndexing.itemOf(testAsset("c", Instant.parse("2024-06-01T00:00:00Z"))),
            )

        assertEquals(listOf(0, 2), SearchIndexing.sectionStarts(items).toList())
        assertFalse(SearchIndexing.sectionStarts(emptyList()).isNotEmpty())
    }

    private fun cluster(
        id: String,
        status: ModelOutputStatus,
        slot: ModelSlot,
        assets: List<String>,
    ) = FaceCluster(
        id = FaceClusterId(id),
        slot = slot,
        status = status.asWire(),
        assetIds = assets.map(::AssetId),
    )
}
