package com.justin13888.capsule.screens.mock

import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.LabelRefusal
import com.justin13888.capsule.screens.domain.LabelTarget
import com.justin13888.capsule.screens.domain.LabelTargetNotFound
import com.justin13888.capsule.screens.domain.RegionShape
import com.justin13888.capsule.screens.domain.ShapeRefusal
import com.justin13888.capsule.screens.domain.ShapeRefusalReason
import com.justin13888.capsule.screens.domain.StackId
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue

/**
 * The mock's label store refuses what the core's workspace refuses, writes nothing on a
 * refusal, and answers every write with what its target holds.
 */
class MockLabelStoreTest {
    private val asset = AssetId("asset-1")
    private val stack = LabelTarget.Stack(StackId("stack-1"))
    private val album = LabelTarget.Album(AlbumId("album-1"))
    private val square = RegionShape.Rect(0.1f, 0.1f, 0.3f, 0.3f)
    private val pastEdge = RegionShape.Rect(0.8f, 0f, 0.5f, 0.5f)

    private fun store(existing: Boolean = true) =
        MockLabelStore(if (existing) MockLabelStore.Existence.EVERYTHING else MockLabelStore.Existence.NOTHING, seed = 7)

    @Test
    fun stackAndAlbumLabelsAreNormalizedDeduplicatedAndSorted() =
        runBlocking {
            val store = store()
            store.addLabel("  zebra ", stack)
            assertEquals(listOf("apple", "zebra"), store.addLabel("apple", stack))
            assertEquals(listOf("apple", "zebra"), store.addLabel("apple", stack))
            assertEquals(listOf("Trip"), store.addLabel("Trip", album))
        }

    @Test
    fun removingALabelTheTargetDoesNotCarryAnswersTheUnchangedList() =
        runBlocking {
            val store = store()
            store.addLabel("kept", album)
            assertEquals(listOf("kept"), store.removeLabel("absent", album))
            assertTrue(store.removeLabel(" kept", album).isEmpty())
        }

    @Test
    fun aRefusedLabelWritesNothing() =
        runBlocking {
            val store = store()
            assertFailsWith<LabelRefusal.Empty> { store.addLabel("   ", stack) }
            assertTrue(store.labels(stack).isEmpty())
        }

    @Test
    fun aTargetTheLibraryDoesNotHoldIsNotFound() =
        runBlocking<Unit> {
            val store = store(existing = false)
            assertFailsWith<LabelTargetNotFound> { store.addLabel("x", stack) }
            assertFailsWith<LabelTargetNotFound> { store.regions(asset) }
            assertFailsWith<LabelTargetNotFound> { store.addRegion(square, emptyList(), asset) }
        }

    @Test
    fun refusalsComeInTheWorkspacesOrderTheLabelOrShapeBeforeTheTarget() =
        runBlocking {
            val store = store(existing = false)
            val missingRegion = LabelTarget.Region(asset, MockIdentifiers.regionId(7, 0))
            assertFailsWith<LabelRefusal.Empty> { store.addLabel(" ", stack) }
            assertFailsWith<LabelRefusal.Empty> { store.removeLabel(" ", album) }
            assertFailsWith<LabelRefusal.Empty> { store.addLabel("", missingRegion) }
            assertFailsWith<LabelRefusal.Empty> { store.addRegion(pastEdge, listOf(" "), asset) }
            assertFailsWith<LabelRefusal.Empty> { store.addRegion(square, listOf(" "), asset) }
            val refusal = assertFailsWith<ShapeRefusal> { store.addRegion(pastEdge, listOf("ok"), asset) }
            assertEquals(ShapeRefusalReason.PAST_EDGE, refusal.reason)
        }

    @Test
    fun aRegionIsWrittenWithItsFirstLabelsAndTakesLabelsOfItsOwn() =
        runBlocking {
            val store = store()
            val region = store.addRegion(square, listOf("face", " Alice"), asset)
            assertEquals(listOf("Alice", "face"), region.labels)
            val target = LabelTarget.Region(asset, region.id)
            assertEquals(listOf("Alice", "face", "smiling"), store.addLabel("smiling", target))
            assertEquals(listOf(listOf("Alice", "face", "smiling")), store.regions(asset).map { it.labels })
        }

    @Test
    fun aRefusedLabelOrShapeLeavesNoRegionBehind() =
        runBlocking {
            val store = store()
            assertFailsWith<LabelRefusal.Empty> { store.addRegion(square, listOf("ok", " "), asset) }
            assertFailsWith<ShapeRefusal> { store.addRegion(pastEdge, emptyList(), asset) }
            assertTrue(store.regions(asset).isEmpty())
        }

    @Test
    fun aDeleteIsFinalAndItsIdIsNeverMintedAgain() =
        runBlocking {
            val store = store()
            val first = store.addRegion(square, listOf("a"), asset)
            store.deleteRegion(first.id, asset)
            assertTrue(store.regions(asset).isEmpty())
            assertFailsWith<LabelTargetNotFound> { store.labels(LabelTarget.Region(asset, first.id)) }
            assertFailsWith<LabelTargetNotFound> { store.deleteRegion(first.id, asset) }
            assertNotEquals(first.id, store.addRegion(square, emptyList(), asset).id)
        }

    @Test
    fun regionIdsAreAFunctionOfTheSeedAndShapedLikeUuidV7() =
        runBlocking {
            val first = store().addRegion(square, emptyList(), asset)
            val second = store().addRegion(square, emptyList(), asset)
            assertEquals(first.id, second.id)
            assertEquals(MockIdentifiers.regionId(7, 0), first.id)
            assertTrue(Regex("[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}").matches(first.id.value))
            assertEquals(64, MockIdentifiers.blobHash(7, 0).value.length)
        }
}
