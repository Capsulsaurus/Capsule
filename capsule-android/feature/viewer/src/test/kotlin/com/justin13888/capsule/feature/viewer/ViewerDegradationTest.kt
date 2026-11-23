package com.justin13888.capsule.feature.viewer

import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.RepresentationTier
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The four conditions an asset record alone establishes, read off the seeded library.
 *
 * Driven through the fixture's landmarks rather than through hand-built records: the fixture
 * forces one asset into each documented state, so these assert against what the repositories
 * actually hand out instead of against a double that could drift from it.
 *
 * The claim under test is narrower than "the record looks odd". Each condition has to be
 * recognised *distinctly*, because each has a different remedy and a different answer to "is
 * my photograph safe?", and only one of them may stop the user editing.
 */
class ViewerDegradationTest {
    private val landmarks = ViewerFixture.LANDMARKS

    @Test
    fun `an ordinary photograph carries no conditions at all`() {
        assertEquals(emptyList<ViewerDegradation>(), intrinsicDegradations(ViewerFixture.ordinaryAsset()))
    }

    @Test
    fun `an asset written by a newer protocol version is recognised and locked for editing`() {
        val asset = assetOf(ViewerFixture.require(landmarks.forwardSchemaAssetId, "forward-schema"))

        val conditions = intrinsicDegradations(asset)

        assertTrue(ViewerDegradation.CreatedByNewerVersion in conditions)
        assertTrue(isForwardSchema(asset))
        assertFalse(ViewerPage.of(asset).isEditable) { "a sidecar this build cannot re-sign must be read-only" }
    }

    @Test
    fun `a format no decoder here claims is recognised without blocking edits`() {
        val asset = assetOf(ViewerFixture.require(landmarks.unreadableAssetId, "unreadable"))

        val conditions = intrinsicDegradations(asset)

        assertTrue(ViewerDegradation.UnreadableHere in conditions)
        assertTrue(isUnreadable(asset))
        // Not being able to draw a file says nothing about whether its metadata may change.
        assertTrue(ViewerPage.of(asset).isEditable)
    }

    @Test
    fun `a refused inbound change is recognised as quarantine, not as a lock`() {
        val asset = assetOf(ViewerFixture.require(landmarks.quarantinedAssetId, "quarantined"))

        val conditions = intrinsicDegradations(asset)

        assertTrue(ViewerDegradation.Quarantined in conditions)
        // The refusal is about what a peer proposed, never about what the user may do.
        assertTrue(ViewerPage.of(asset).isEditable)
    }

    @Test
    fun `an original still uploading elsewhere is recognised as pending`() {
        val asset = assetOf(ViewerFixture.require(landmarks.awaitingOriginalAssetId, "awaiting-original"))

        assertTrue(ViewerDegradation.OriginalOnAnotherDevice in intrinsicDegradations(asset))
    }

    @Test
    fun `only the forward-schema condition stops an edit`() {
        val blocking = ViewerDegradation.entries.filter(ViewerDegradation::blocksEditing)

        assertEquals(listOf(ViewerDegradation.CreatedByNewerVersion), blocking)
    }

    @Test
    fun `the drawable rung never falls below the always-present placeholder`() {
        val asset = assetOf(ViewerFixture.require(landmarks.degradedAssetId, "degraded"))

        assertEquals(RepresentationTier.Lqip, drawableTier(asset))
    }

    private fun assetOf(id: AssetId) = requireNotNull(ViewerFixture.LIBRARY.assetsById[id])
}
