package com.justin13888.capsule.screens.labels

import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.LabelRefusal
import com.justin13888.capsule.screens.domain.LabelTarget
import com.justin13888.capsule.screens.domain.LabelTargetNotFound
import com.justin13888.capsule.screens.domain.RegionId
import com.justin13888.capsule.screens.domain.RegionShape
import com.justin13888.capsule.screens.domain.ShapeRefusal
import com.justin13888.capsule.screens.domain.ShapeRefusalReason
import com.justin13888.capsule.screens.domain.StackId
import com.justin13888.capsule.screens.labels.FfiLabelPort.Companion.toDomain
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import uniffi.capsule_sdk.FfiException
import uniffi.capsule_sdk.FfiLabelRefusal
import uniffi.capsule_sdk.FfiWorkspace
import java.nio.file.Files
import java.util.UUID
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertTrue

/**
 * The label port over a real `FfiWorkspace` (JNA loads the host `libcapsule_sdk`, as the
 * `:core` smoke does): every verb reaches the workspace, every write answers with what
 * the target holds, and each of the workspace's refusals arrives as the domain's.
 */
class FfiLabelPortTest {
    private val root = Files.createTempDirectory("capsule-android-labels")
    private val workspace =
        FfiWorkspace.create(
            root.toString(),
            "correct horse".toByteArray(),
            FfiDeviceTier.NORMAL,
            FfiClientBuild("capsule-android", "0.0.0"),
        )
    private val album = AlbumId(workspace.ensureAlbum(workspace.defaultAlbumId(), "Camera Roll"))
    private val asset = AssetId(workspace.sealAsset(album.value, "IMG_0042.JPG", "photo".toByteArray()))
    private val port = FfiLabelPort(workspace, Dispatchers.Unconfined)
    private val circle = RegionShape.Circle(0.5f, 0.5f, 0.25f)
    private val missing = UUID.randomUUID().toString()

    @AfterTest
    fun cleanUp() {
        workspace.close()
        root.toFile().deleteRecursively()
    }

    @Test
    fun aRegionIsMarkedLabelledAndDeletedForGood() =
        runBlocking<Unit> {
            assertEquals(emptyList(), port.regions(asset))
            val first = port.addRegion(circle, listOf("  Zoe ", "Ada"), asset)
            assertEquals(circle, first.shape)
            assertEquals(listOf("Ada", "Zoe"), first.labels)
            val rect = RegionShape.Rect(0f, 0f, 0.5f, 1f)
            val second = port.addRegion(rect, emptyList(), asset)
            assertEquals(listOf(first, second), port.regions(asset))

            val target = LabelTarget.Region(asset, first.id)
            assertEquals(listOf("Ada", "Cy", "Zoe"), port.addLabel("Cy", target))
            assertEquals(listOf("Ada", "Cy"), port.removeLabel("Zoe", target))
            assertEquals(listOf("Ada", "Cy"), port.removeLabel("Nobody", target))
            assertEquals(listOf("Ada", "Cy"), port.labels(target))

            port.deleteRegion(first.id, asset)
            assertEquals(listOf(second), port.regions(asset))
            val gone = assertFailsWith<LabelTargetNotFound> { port.labels(target) }
            assertEquals(FfiLabelRefusal.NotFound, assertIs<FfiException.Label>(gone.cause).refusal)
            assertFailsWith<LabelTargetNotFound> { port.addLabel("Ada", target) }
            assertFailsWith<LabelTargetNotFound> { port.deleteRegion(first.id, asset) }
        }

    @Test
    fun anAlbumsLabelsAreNormalizedSortedAndRemoved() =
        runBlocking<Unit> {
            val target = LabelTarget.Album(album)
            assertEquals(emptyList(), port.labels(target))
            assertEquals(listOf("Trips"), port.addLabel(" Trips ", target))
            assertEquals(listOf("Family", "Trips"), port.addLabel("Family", target))
            assertEquals(listOf("Family"), port.removeLabel("Trips", target))
        }

    @Test
    fun theWorkspacesShapeRefusalsArriveAsTheDomainsAndWriteNothing() =
        runBlocking<Unit> {
            val pastEdge = RegionShape.Circle(0.1f, 0.5f, 0.25f)
            val empty = RegionShape.Rect(0.1f, 0.1f, 0f, 0.5f)
            val outside = RegionShape.Rect(-0.1f, 0.1f, 0.2f, 0.5f)
            val refused = assertFailsWith<ShapeRefusal> { port.addRegion(pastEdge, emptyList(), asset) }
            assertEquals(ShapeRefusal(ShapeRefusalReason.PAST_EDGE), refused)
            val cause = assertIs<FfiException.Label>(refused.cause)
            assertEquals(FfiLabelRefusal.RegionPastEdge, cause.refusal)
            assertEquals(
                ShapeRefusal(ShapeRefusalReason.EMPTY),
                assertFailsWith<ShapeRefusal> { port.addRegion(empty, emptyList(), asset) },
            )
            assertEquals(
                ShapeRefusal(ShapeRefusalReason.OUT_OF_RANGE),
                assertFailsWith<ShapeRefusal> { port.addRegion(outside, emptyList(), asset) },
            )
            assertTrue(port.regions(asset).isEmpty())
        }

    @Test
    fun aRefusedLabelIsRefusedBeforeAnUnknownTargetAndLeavesNoRegion() =
        runBlocking<Unit> {
            assertFailsWith<LabelRefusal.Empty> { port.addRegion(circle, listOf("Ada", "  "), asset) }
            assertEquals(
                LabelRefusal.TooLong(257),
                assertFailsWith<LabelRefusal.TooLong> { port.addRegion(circle, listOf("x".repeat(257)), asset) },
            )
            assertTrue(port.regions(asset).isEmpty())
            assertFailsWith<LabelRefusal.Empty> { port.addLabel("\n", LabelTarget.Stack(StackId(missing))) }
            assertFailsWith<LabelRefusal.Empty> { port.removeLabel(" ", LabelTarget.Album(AlbumId(missing))) }
        }

    @Test
    fun aTargetTheWorkspaceDoesNotHoldIsNotFoundAndSoIsAnIdThatIsNoUuid() =
        runBlocking<Unit> {
            assertFailsWith<LabelTargetNotFound> { port.regions(AssetId(missing)) }
            assertFailsWith<LabelTargetNotFound> { port.addRegion(circle, emptyList(), AssetId(missing)) }
            assertFailsWith<LabelTargetNotFound> { port.labels(LabelTarget.Stack(StackId(missing))) }
            assertFailsWith<LabelTargetNotFound> { port.addLabel("Trips", LabelTarget.Album(AlbumId(missing))) }
            assertFailsWith<LabelTargetNotFound> { port.labels(LabelTarget.Region(asset, RegionId("not-a-uuid"))) }
            assertFailsWith<LabelTargetNotFound> { port.regions(AssetId("elsewhere")) }
        }

    @Test
    fun everyRefusalOfTheWorkspaceHasItsDomainCounterpart() {
        assertEquals(LabelTargetNotFound(), FfiLabelRefusal.NotFound.toDomain())
        assertEquals(LabelRefusal.Empty(), FfiLabelRefusal.LabelEmpty.toDomain())
        assertEquals(LabelRefusal.TooLong(300), FfiLabelRefusal.LabelTooLong(300uL).toDomain())
        assertEquals(LabelRefusal.TooLong(Int.MAX_VALUE), FfiLabelRefusal.LabelTooLong(ULong.MAX_VALUE).toDomain())
        assertEquals(ShapeRefusal(ShapeRefusalReason.OUT_OF_RANGE), FfiLabelRefusal.RegionOutOfRange.toDomain())
        assertEquals(ShapeRefusal(ShapeRefusalReason.EMPTY), FfiLabelRefusal.RegionEmpty.toDomain())
        assertEquals(ShapeRefusal(ShapeRefusalReason.PAST_EDGE), FfiLabelRefusal.RegionPastEdge.toDomain())
    }
}
