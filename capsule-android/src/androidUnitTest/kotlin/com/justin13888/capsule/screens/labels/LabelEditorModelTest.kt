package com.justin13888.capsule.screens.labels

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.AssetRegion
import com.justin13888.capsule.screens.domain.LabelPort
import com.justin13888.capsule.screens.domain.LabelRefusal
import com.justin13888.capsule.screens.domain.LabelTarget
import com.justin13888.capsule.screens.domain.RegionId
import com.justin13888.capsule.screens.domain.RegionShape
import com.justin13888.capsule.screens.domain.ShapeRefusal
import com.justin13888.capsule.screens.domain.ShapeRefusalReason
import com.justin13888.capsule.screens.domain.StackId
import com.justin13888.capsule.screens.mock.MockLabelStore
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

/**
 * The editor writes through the port, redraws from its answers, and leaves its state as
 * it was when a write is refused. `runBlocking` runs on one thread, which is the
 * main-confinement the model is written for.
 */
class LabelEditorModelTest {
    private val asset = AssetId("asset-1")
    private val stack = StackId("stack-1")
    private val left = RegionShape.Rect(0f, 0f, 0.5f, 0.5f)
    private val right = RegionShape.Rect(0.5f, 0.5f, 0.5f, 0.5f)

    private fun port() = MockLabelStore(MockLabelStore.Existence.EVERYTHING, seed = 3)

    @Test
    fun onAStackLabelsAreAddedAndRemovedAsThePortStoresThem() =
        runBlocking {
            val model = LabelEditorModel(LabelSubject.Stack(stack), port())
            model.load()
            assertEquals(LabelTarget.Stack(stack), model.labelTarget)
            model.addLabel("  Holiday ")
            model.addLabel("beach")
            assertEquals(listOf("Holiday", "beach"), model.state.value.labels)
            model.removeLabel("Holiday")
            assertEquals(listOf("beach"), model.state.value.labels)
            assertNull(model.state.value.failure)
        }

    @Test
    fun onAnAssetWithNoRegionSelectedATypedLabelGoesNowhere() =
        runBlocking {
            val model = LabelEditorModel(LabelSubject.Asset(asset), port())
            model.load()
            assertNull(model.labelTarget)
            model.addLabel("ignored")
            assertEquals(emptyList(), model.state.value.labels)
        }

    @Test
    fun drawingWritesTheDraftAsARegionSelectsItAndLabelsGoToIt() =
        runBlocking {
            val model = LabelEditorModel(LabelSubject.Asset(asset), port())
            model.load()
            model.startDrawing(RegionDraft.Kind.CIRCLE)
            model.updateDraft(model.state.value.draft!!.moved(0.1, 0.0))
            model.finishDrawing(listOf("face"))
            val state = model.state.value
            assertNull(state.draft)
            val region = state.regions.single()
            assertEquals(RegionDraft.Circle(0.6, 0.5, 0.16).shape, region.shape)
            assertEquals(region.id, state.selectedRegionId)
            assertEquals(listOf("face"), state.labels)
            model.addLabel("Alice")
            assertEquals(listOf("Alice", "face"), model.state.value.labels)
            assertEquals(listOf("Alice", "face"), model.state.value.regions.single().labels)
        }

    @Test
    fun reloadingKeepsTheSelectionAndReadsTheStoresRegions() =
        runBlocking {
            val store = port()
            val first = store.addRegion(left, listOf("a"), asset)
            val second = store.addRegion(right, listOf("b"), asset)
            val model = LabelEditorModel(LabelSubject.Asset(asset), store)
            model.load()
            assertEquals(first.id, model.state.value.selectedRegionId)
            model.selectRegion(second.id)
            assertEquals(listOf("b"), model.state.value.labels)
            model.load()
            assertEquals(second.id, model.state.value.selectedRegionId)
        }

    @Test
    fun deletingTheSelectedRegionSelectsTheNextOne() =
        runBlocking {
            val store = port()
            store.addRegion(left, emptyList(), asset)
            val second = store.addRegion(right, listOf("b"), asset)
            val model = LabelEditorModel(LabelSubject.Asset(asset), store)
            model.load()
            model.deleteSelectedRegion()
            assertEquals(listOf(second.id), model.state.value.regions.map { it.id })
            assertEquals(second.id, model.state.value.selectedRegionId)
            assertEquals(listOf("b"), model.state.value.labels)
        }

    @Test
    fun aRefusedLabelIsNamedAndChangesNothing() =
        runBlocking {
            val model = LabelEditorModel(LabelSubject.Stack(stack), port())
            model.addLabel("kept")
            model.addLabel("   ")
            assertEquals(LabelFailure.LabelEmpty, model.state.value.failure)
            assertEquals(listOf("kept"), model.state.value.labels)
            model.addLabel("next")
            assertNull(model.state.value.failure)
        }

    @Test
    fun aMissingSubjectIsNamedAsNotFound() =
        runBlocking {
            val model = LabelEditorModel(LabelSubject.Stack(stack), MockLabelStore(MockLabelStore.Existence.NOTHING, seed = 3))
            model.load()
            assertEquals(LabelFailure.NotFound, model.state.value.failure)
            assertEquals(false, model.state.value.isWriting)
        }

    @Test
    fun errorsMapOntoTheEditorsFailures() {
        assertEquals(LabelFailure.LabelTooLong(300), LabelEditorModel.failureFor(LabelRefusal.TooLong(300)))
        assertEquals(
            LabelFailure.ShapeRefused(ShapeRefusalReason.PAST_EDGE),
            LabelEditorModel.failureFor(ShapeRefusal(ShapeRefusalReason.PAST_EDGE)),
        )
        assertEquals(LabelFailure.Unavailable, LabelEditorModel.failureFor(IllegalStateException("port down")))
        assertEquals(LabelFailure.Unavailable, LabelEditorModel.failureFor(CancellationException()))
    }

    @Test
    fun whileALabelWriteIsInFlightSelectionAndDrawingAreRefusedAndTheAnswerLandsOnItsRegion() =
        runBlocking {
            val store = port()
            val first = store.addRegion(left, listOf("a"), asset)
            val second = store.addRegion(right, listOf("b"), asset)
            val gated = GatedLabelPort(store)
            val model = LabelEditorModel(LabelSubject.Asset(asset), gated)
            model.load()
            assertEquals(first.id, model.state.value.selectedRegionId)
            val write = launch { model.addLabel("x") }
            gated.entered.await()
            model.selectRegion(second.id)
            model.startDrawing(RegionDraft.Kind.RECT)
            assertEquals(first.id, model.state.value.selectedRegionId)
            assertNull(model.state.value.draft)
            gated.open.complete(Unit)
            write.join()
            assertEquals(first.id, model.state.value.selectedRegionId)
            assertEquals(listOf("a", "x"), model.state.value.labels)
            assertEquals(listOf(listOf("a", "x"), listOf("b")), model.state.value.regions.map { it.labels })
            model.selectRegion(second.id)
            assertEquals(listOf("b"), model.state.value.labels)
        }

    @Test
    fun finishingARegionKeepsADraftThatReplacedItDuringTheWrite() =
        runBlocking {
            val gated = GatedLabelPort(port())
            val model = LabelEditorModel(LabelSubject.Asset(asset), gated)
            model.load()
            model.startDrawing(RegionDraft.Kind.RECT)
            val write = launch { model.finishDrawing(listOf("face")) }
            gated.entered.await()
            val newer = RegionDraft.of(RegionDraft.Kind.CIRCLE)
            model.updateDraft(newer)
            gated.open.complete(Unit)
            write.join()
            assertEquals(1, model.state.value.regions.size)
            assertEquals(newer, model.state.value.draft)
        }

    @Test
    fun aStackOrAlbumHasNoRegionsToDraw() {
        val model = LabelEditorModel(LabelSubject.Stack(stack), port())
        model.startDrawing(RegionDraft.Kind.RECT)
        assertNull(model.state.value.draft)
    }

    @Test
    fun cancellingADraftWritesNothing() =
        runBlocking {
            val store = port()
            val model = LabelEditorModel(LabelSubject.Asset(asset), store)
            model.startDrawing(RegionDraft.Kind.RECT)
            model.cancelDrawing()
            model.finishDrawing(listOf("never"))
            assertEquals(emptyList(), store.regions(asset))
        }

    /** Forwards to a mock store, holding every write until the test opens the gate. */
    private class GatedLabelPort(
        private val inner: LabelPort,
    ) : LabelPort {
        val entered = CompletableDeferred<Unit>()
        val open = CompletableDeferred<Unit>()

        private suspend fun pass() {
            entered.complete(Unit)
            open.await()
        }

        override suspend fun labels(target: LabelTarget): List<String> = inner.labels(target)

        override suspend fun addLabel(
            label: String,
            target: LabelTarget,
        ): List<String> {
            pass()
            return inner.addLabel(label, target)
        }

        override suspend fun removeLabel(
            label: String,
            target: LabelTarget,
        ): List<String> {
            pass()
            return inner.removeLabel(label, target)
        }

        override suspend fun regions(asset: AssetId): List<AssetRegion> = inner.regions(asset)

        override suspend fun addRegion(
            shape: RegionShape,
            labels: List<String>,
            asset: AssetId,
        ): AssetRegion {
            pass()
            return inner.addRegion(shape, labels, asset)
        }

        override suspend fun deleteRegion(
            region: RegionId,
            asset: AssetId,
        ) {
            pass()
            inner.deleteRegion(region, asset)
        }
    }
}
