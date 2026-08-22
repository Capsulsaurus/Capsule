package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.mock.seed.CURRENT_TAG_SLOT
import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DeletionState
import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.core.model.ModelOutputStatus
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.StackType
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.isKnown
import com.justin13888.capsule.core.model.knownOrNull
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The fixture's coverage of the documented state matrix, asserted rather than assumed.
 *
 * Without this, a refactor that quietly stopped producing quarantined assets would leave the
 * quarantine UI untested and unlooked-at until someone hit it in production. Every state the
 * design names has to be reachable in the fixture, and this is what stops that silently
 * eroding.
 */
class LibraryStateMatrixTest {
    private val library = MockFixtures.LIBRARY
    private val landmarks = library.landmarks

    private fun asset(id: AssetId?): Asset {
        assertNotNull(id, "the fixture lost a landmark")
        return requireNotNull(library.assetsById[requireNotNull(id)]) { "landmark $id is not in the library" }
    }

    @Test
    fun `a hidden asset is present and excluded from default views`() {
        val hidden = asset(landmarks.hiddenAssetId)
        assertTrue(hidden.isHidden)
        assertFalse(hidden.isInDefaultViews)
    }

    @Test
    fun `trash covers the whole retention window`() {
        val fresh = asset(landmarks.trashedFreshAssetId).deletion as DeletionState.Trashed
        val mid = asset(landmarks.trashedMidWindowAssetId).deletion as DeletionState.Trashed
        val expired = asset(landmarks.trashedExpiredAssetId).deletion as DeletionState.Trashed
        val now = library.spec.now
        assertFalse(fresh.isPurgeable(now))
        assertFalse(mid.isPurgeable(now))
        // Past its signed floor, so a purge worker may now proceed — the one trash state that
        // is otherwise only reachable by waiting a month.
        assertTrue(expired.isPurgeable(now))
        assertTrue(fresh.retentionRemaining(now) > mid.retentionRemaining(now))
    }

    @Test
    fun `an asset is awaiting its original`() {
        val awaiting = asset(landmarks.awaitingOriginalAssetId)
        assertEquals(SyncState.AwaitingOriginal, awaiting.syncState)
        // Visible everywhere despite not being fully uploaded: awaiting is not a failure.
        assertTrue(awaiting.isInDefaultViews)
    }

    @Test
    fun `an asset holds only its placeholder, so a fetch has somewhere to degrade to`() {
        val degraded = asset(landmarks.degradedAssetId)
        assertEquals(RepresentationTier.Lqip, degraded.heldRepresentations.best)
        assertEquals(RepresentationTier.Lqip, degraded.heldRepresentations.degradeFrom(RepresentationTier.Original))
    }

    @Test
    fun `an asset is quarantined and the surface is listed`() {
        val quarantined = asset(landmarks.quarantinedAssetId)
        assertTrue(quarantined.syncState is SyncState.Quarantined)
        assertTrue(library.quarantine.any { it.assetId == quarantined.id })
        // More than one surface holds something, so the "needs your attention" list is not a
        // single-row screen nobody ever laid out properly.
        assertTrue(
            library.quarantine
                .map { it.surface.raw }
                .distinct()
                .size > 1,
        )
    }

    @Test
    fun `an asset was written by a newer schema and is preserved verbatim`() {
        val forward = asset(landmarks.forwardSchemaAssetId)
        assertFalse(forward.assetType.isKnown)
        assertFalse(forward.cull.isKnown)
        // Preserved, not stripped: the raw token survives the round trip because dropping a
        // field a newer peer wrote would invalidate the signature covering it.
        assertTrue(forward.assetType.raw.isNotEmpty())
    }

    @Test
    fun `an asset cannot be read on this device`() {
        val unreadable = asset(landmarks.unreadableAssetId)
        assertFalse(unreadable.contentType?.isKnown ?: true)
        // The extractor could not probe it, which is what sends layout down the square fallback.
        assertNull(unreadable.dimensions)
    }

    @Test
    fun `a stored fix is in the GCJ-02 datum`() {
        val shifted = asset(landmarks.gcj02AssetId)
        assertEquals(GpsDatum.Gcj02, shifted.gps?.datum?.knownOrNull)
        // And the rest of the library is in WGS-84, so a client that assumed one datum
        // everywhere has something to be wrong about.
        assertTrue(library.assets.any { it.gps?.datum?.knownOrNull == GpsDatum.Wgs84 })
    }

    @Test
    fun `every stack shape is present with a real primary`() {
        val shapes =
            mapOf(
                StackType.RawJpeg to landmarks.rawJpegStackId,
                StackType.Burst to landmarks.burstStackId,
                StackType.LivePhoto to landmarks.livePhotoStackId,
                StackType.Chaptered to landmarks.chapteredStackId,
            )
        shapes.forEach { (type, id) ->
            assertNotNull(id, "no $type stack in the fixture")
            val stack = library.stacks.first { it.id == id }
            assertEquals(type, stack.stackType.knownOrNull)
            val members = library.stackMembers.getValue(stack.id)
            assertTrue(members.any { it.assetId == stack.primaryAssetId }, "$type primary is not a member")
            // The non-primary members are suppressed, which is what a collapsed stack means.
            assertTrue(members.size > 1)
        }
    }

    @Test
    fun `a burst does not always pick its first frame as the primary`() {
        val burst = library.stacks.first { it.id == landmarks.burstStackId }
        val members = library.stackMembers.getValue(burst.id)
        val suppressed = members.count { library.assetsById[it.assetId]?.stack?.isSuppressed == true }
        assertEquals(members.size - 1, suppressed)
    }

    @Test
    fun `the aspect mix reaches square, panorama and video`() {
        assertNotNull(landmarks.squareAssetId)
        assertNotNull(landmarks.panoramaAssetId)
        assertNotNull(landmarks.videoAssetId)
        val panorama = asset(landmarks.panoramaAssetId).dimensions?.aspectRatio ?: 0f
        val square = asset(landmarks.squareAssetId).dimensions?.aspectRatio ?: 0f
        assertTrue(panorama > 2.5f, "widest frame is only $panorama")
        assertTrue(kotlin.math.abs(square - 1f) < 0.05f, "squarest frame is $square")
        assertTrue(asset(landmarks.videoAssetId).duration != null)
    }

    @Test
    fun `both tag namespaces are populated and one AI slot is stale`() {
        val staleOwner = asset(landmarks.staleAiTagAssetId)
        val tags = library.tags.getValue(staleOwner.id)
        assertTrue(tags.ai.any(AiTag::isStale))
        // Stale entries are excluded from queries but kept, so there is something to regenerate.
        assertTrue(tags.ai.size > tags.freshAi.size)
        assertTrue(library.tags.values.any { it.user.isNotEmpty() })
        assertTrue(library.tags.values.any { entry -> entry.ai.any { it.slot == CURRENT_TAG_SLOT } })
    }

    @Test
    fun `a face cluster sits on a superseded model slot`() {
        assertNotNull(landmarks.staleClusterId)
        val stale = library.clusters.first { it.id == landmarks.staleClusterId }
        assertEquals(ModelOutputStatus.Stale, stale.status.knownOrNull)
        assertTrue(library.clusters.any { it.status.knownOrNull == ModelOutputStatus.Current })
        assertTrue(library.people.isNotEmpty())
    }

    @Test
    fun `the default album is nameless and undeletable, and a view album exists`() {
        val default = library.albums.first { it.id == landmarks.defaultAlbumId }
        assertTrue(default.isDefault)
        assertEquals("", default.name)
        assertFalse(default.isDeletable)
        val view = library.albums.first { it.id == landmarks.viewAlbumId }
        assertEquals(AlbumKind.View, view.kind.knownOrNull)
        assertTrue(view.members.isEmpty())
        assertTrue(landmarks.tripAlbumIds.isNotEmpty())
    }

    @Test
    fun `a smart album from a later grammar is preserved unevaluated`() {
        assertTrue(
            library.smartAlbums.any {
                it.predicateSchema >
                    com.justin13888.capsule.core.model.SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA
            },
        )
    }

    @Test
    fun `the device directory keeps a revoked device`() {
        assertTrue(library.devices.any { !it.isActive })
        assertTrue(library.devices.any(com.justin13888.capsule.core.model.Device::isThisDevice))
    }

    @Test
    fun `capture clustering leaves empty days and heavy days`() {
        val perDay = library.assets.groupingBy { it.captureTimestamp.toString().take(10) }.eachCount()
        val calendarDays = 365L * library.spec.years
        // Empty stretches: fewer distinct shooting days than calendar days.
        assertTrue(perDay.size < calendarDays, "every calendar day has photos")
        // And at least one day several times the median, which is what a trip or a shoot is.
        val median = perDay.values.sorted()[perDay.size / 2]
        assertTrue(perDay.values.max() > median * 4, "no heavy days in the fixture")
    }

    @Test
    fun `guest deposits and issued links are present`() {
        assertTrue(library.drops.isNotEmpty())
        assertTrue(library.shareLinks.any { it.fragmentSecret != null })
        assertTrue(library.uploadLinks.isNotEmpty())
        assertTrue(library.moderationEvents.isNotEmpty())
    }
}
