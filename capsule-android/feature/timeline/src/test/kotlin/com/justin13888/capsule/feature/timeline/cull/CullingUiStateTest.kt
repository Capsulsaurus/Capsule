package com.justin13888.capsule.feature.timeline.cull

import androidx.compose.ui.graphics.Color
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.ui.gallery.GalleryCellMarkers
import com.justin13888.capsule.feature.timeline.TimelineCell
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Instant

/**
 * The culling pass as a state machine, with no renderer anywhere near it.
 *
 * Every transition is pure, which is the whole reason to have written it that way: a review
 * that loses the photographer's place, advances past the end, or offers to sweep something it
 * should not is a bug that has to be catchable without a screen.
 */
class CullingUiStateTest {
    private fun frame(
        asset: Int,
        flag: CullFlag?,
    ) = CullFrame(
        asset = asset,
        cell =
            TimelineCell(
                assetId = AssetId("asset-$asset"),
                markers = GalleryCellMarkers.None,
                degradation = null,
                placeholder = Color.Gray,
                capturedAt = Instant.EPOCH,
                isVideo = false,
                cull = flag,
                rating = 0,
            ),
        flag = flag,
    )

    private val session =
        CullingUiState.over(
            frames =
                listOf(
                    frame(10, CullFlag.Pick),
                    frame(11, CullFlag.Neutral),
                    frame(12, CullFlag.Reject),
                    frame(13, CullFlag.Neutral),
                    frame(14, null),
                ),
            origin = 10,
        )

    @Test
    fun `a review opens on the frame it was entered from`() {
        assertEquals(10, session.current?.asset)
        assertEquals(5, session.counts.total)
        assertEquals(1, session.counts.picks)
        assertEquals(2, session.counts.undecided)
        assertEquals(1, session.counts.rejects)
    }

    @Test
    fun `an unrecognised token counts as no decision at all`() {
        // Deliberately not folded into `undecided`: a value a newer peer wrote is not a
        // statement that nothing was decided, and counting it as one would let a sweep act on
        // a decision this build cannot read.
        assertEquals(1 + 2 + 1, session.counts.picks + session.counts.undecided + session.counts.rejects)
        assertEquals(5, session.counts.total)
        assertNull(session.frames.last().flag)
    }

    @Test
    fun `a filter restricts the visible set without losing the frame under review`() {
        val onNeutral = session.movedTo(3).withFilter(CullFilter.Undecided)

        assertEquals(listOf(11, 13), onNeutral.visible.map(CullFrame::asset))
        assertEquals(13, onNeutral.current?.asset, "the frame in hand must survive the lens change")
    }

    @Test
    fun `a filter that excludes the frame under review lands somewhere real`() {
        val onRejects = session.withFilter(CullFilter.Rejects)

        assertEquals(listOf(12), onRejects.visible.map(CullFrame::asset))
        assertEquals(12, onRejects.current?.asset)
    }

    @Test
    fun `flagging under a flag filter lands on the neighbour rather than off the end`() {
        val onNeutral = session.withFilter(CullFilter.Undecided)
        assertEquals(11, onNeutral.current?.asset)

        val rejected = onNeutral.withFlag(CullFlag.Reject)

        assertEquals(listOf(13), rejected.visible.map(CullFrame::asset))
        assertEquals(13, rejected.current?.asset)
        assertEquals(2, rejected.counts.rejects)
    }

    @Test
    fun `movement is clamped at both ends so a fast pass cannot run off the set`() {
        assertEquals(10, session.movedBy(-5).current?.asset)
        assertEquals(14, session.movedBy(99).current?.asset)
        assertEquals(4, session.movedBy(99).position)
    }

    @Test
    fun `the sweep names exactly the rejected frames and never an unknown one`() {
        val swept = session.movedTo(4).withFlag(CullFlag.Reject)

        assertEquals(
            listOf(AssetId("asset-12"), AssetId("asset-14")),
            swept.rejectedAssets(),
        )
    }

    @Test
    fun `a completed sweep removes what it took and re-clamps the position`() {
        val afterSweep = session.movedTo(2).without(setOf(AssetId("asset-12")))

        assertEquals(listOf(10, 11, 13, 14), afterSweep.frames.map(CullFrame::asset))
        assertEquals(13, afterSweep.current?.asset)
        assertFalse(afterSweep.isConfirmingSweep, "a finished sweep must not leave its confirmation up")
    }

    @Test
    fun `a filter matching nothing reports empty rather than pretending to show a frame`() {
        val nothingPicked = CullingUiState.over(listOf(frame(1, CullFlag.Reject)), origin = 1)
        val onPicks = nothingPicked.withFilter(CullFilter.Picks)

        assertTrue(onPicks.isEmpty)
        assertNull(onPicks.current)
        assertEquals(0, onPicks.position)
    }

    @Test
    fun `an empty review is a legal state and not a crash`() {
        assertTrue(CullingUiState.EMPTY.isEmpty)
        assertNull(CullingUiState.EMPTY.current)
        assertEquals(emptyList<AssetId>(), CullingUiState.EMPTY.rejectedAssets())
        assertEquals(CullingUiState.EMPTY, CullingUiState.EMPTY.withFlag(CullFlag.Pick))
    }
}
