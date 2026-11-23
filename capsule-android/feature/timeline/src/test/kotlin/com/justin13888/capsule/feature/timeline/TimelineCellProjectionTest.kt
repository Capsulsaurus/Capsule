package com.justin13888.capsule.feature.timeline

import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.QuarantineSurface
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.ui.gallery.GalleryCellSync
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Duration

/**
 * What a tile says about an asset, asserted against the fixture's own landmark states.
 *
 * The projection is pure, so every one of these is answerable without a renderer — which is
 * the point of projecting at all. Driven from the seeded library rather than from hand-built
 * assets so that a change to what the fixture calls "degraded" is caught here rather than
 * discovered by eye three screens later.
 */
class TimelineCellProjectionTest {
    private val mock = TimelineFixture.repositories()
    private val landmarks = mock.backend.library.landmarks

    @Test
    fun `an asset awaiting its original is marked as such and not as a lost one`() {
        val asset = TimelineFixture.landmark(mock, "awaiting-original", landmarks.awaitingOriginalAssetId)
        val cell = asset.toTimelineCell()

        assertEquals(TimelineDegradation.AwaitingOriginal, cell.degradation)
        assertTrue(cell.markers.isDegraded)
        // Pending, not failed: the original is coming, and a failure marker would be a lie
        // that invites the user to re-import something that is already on its way.
        assertEquals(GalleryCellSync.Pending, cell.markers.sync)
    }

    @Test
    fun `an asset holding only its placeholder reports full resolution unavailable`() {
        val asset = TimelineFixture.landmark(mock, "degraded", landmarks.degradedAssetId)
        assertEquals(HeldRepresentations.LQIP_ONLY, asset.heldRepresentations)

        assertEquals(TimelineDegradation.FullResolutionUnavailable, asset.toTimelineCell().degradation)
    }

    @Test
    fun `a format no decoder here claims reports as unreadable on this device`() {
        val asset = TimelineFixture.landmark(mock, "unreadable", landmarks.unreadableAssetId)
        assertTrue(asset.contentType is Wire.Unknown)

        // Unreadable outranks full-resolution-unavailable even though both apply: the codec is
        // the reason, and telling the user to free up a download would send them nowhere.
        assertEquals(TimelineDegradation.UnreadableOnThisDevice, asset.toTimelineCell().degradation)
    }

    @Test
    fun `a quarantined asset outranks every other degraded state`() {
        val asset = TimelineFixture.landmark(mock, "quarantined", landmarks.quarantinedAssetId)
        val alsoMissingEverything =
            asset.copy(
                heldRepresentations = HeldRepresentations.LQIP_ONLY,
                contentType = Wire.Unknown("image/x-nothing"),
                syncState = SyncState.Quarantined(QuarantineSurface.StaleRevival),
            )

        assertEquals(TimelineDegradation.Quarantined, alsoMissingEverything.toTimelineCell().degradation)
        assertEquals(GalleryCellSync.Failed, alsoMissingEverything.toTimelineCell().markers.sync)
    }

    @Test
    fun `an ordinary asset carries no degraded marker at all`() {
        val asset =
            mock.backend.library.assets.first {
                it.heldRepresentations.holds(RepresentationTier.Thumbnail) &&
                    it.syncState == SyncState.Synced &&
                    it.contentType !is Wire.Unknown
            }
        val cell = asset.toTimelineCell()

        assertNull(cell.degradation)
        assertFalse(cell.markers.isDegraded)
        assertEquals(GalleryCellSync.Synced, cell.markers.sync)
    }

    @Test
    fun `a stack badge appears only when the stack really holds more than one frame`() {
        val stacked =
            mock.backend.library.assets
                .first { it.stack != null }

        assertNull(stacked.toTimelineCell(stackSize = 1).markers.stackCount, "a stack of one is not a stack")
        assertEquals(4, stacked.toTimelineCell(stackSize = 4).markers.stackCount)
        assertNull(stacked.toTimelineCell().markers.stackCount, "an unlooked-up stack draws no badge")
    }

    @Test
    fun `a clip carries its running time and a still carries none`() {
        val video = TimelineFixture.landmark(mock, "video", landmarks.videoAssetId)
        val still =
            mock.backend.library.assets
                .first { it.duration == null }

        assertNotNull(video.toTimelineCell().markers.videoDuration)
        assertTrue(video.toTimelineCell().isVideo)
        assertNull(still.toTimelineCell().markers.videoDuration)
        assertFalse(still.toTimelineCell().isVideo)
    }

    @Test
    fun `a running time reads as minutes until it passes the hour`() {
        assertEquals("0:07", formatRunningTime(Duration.ofSeconds(7)))
        assertEquals("1:23", formatRunningTime(Duration.ofSeconds(83)))
        assertEquals("12:00", formatRunningTime(Duration.ofMinutes(12)))
        assertEquals("1:02:03", formatRunningTime(Duration.ofSeconds(3_723)))
        assertEquals("0:00", formatRunningTime(Duration.ofSeconds(-5)))
    }

    @Test
    fun `favourite is the top of the rating scale and nothing else`() {
        val asset =
            mock.backend.library.assets
                .first()

        assertTrue(asset.copy(rating = FAVOURITE_RATING).toTimelineCell().isFavourite)
        assertTrue(
            asset
                .copy(rating = FAVOURITE_RATING)
                .toTimelineCell()
                .markers.isFavourite,
        )
        assertFalse(asset.copy(rating = FAVOURITE_RATING - 1).toTimelineCell().isFavourite)
    }

    @Test
    fun `a library that never had a server draws no sync marker on every single tile`() {
        val asset =
            mock.backend.library.assets
                .first()
                .copy(syncState = SyncState.LocalOnly)

        // Local-only is a valid mode, not a degradation: a marker on every tile in the grid
        // would be chrome rather than information.
        assertEquals(GalleryCellSync.None, asset.toTimelineCell().markers.sync)
        assertNull(asset.toTimelineCell().degradation)
    }

    @Test
    fun `a cull token from a newer peer is preserved rather than read as undecided`() {
        val forward = TimelineFixture.landmark(mock, "forward-schema", landmarks.forwardSchemaAssetId)

        assertTrue(forward.cull is Wire.Unknown)
        assertNull(forward.toTimelineCell().cull, "an unknown token must not masquerade as a decision")
    }
}
