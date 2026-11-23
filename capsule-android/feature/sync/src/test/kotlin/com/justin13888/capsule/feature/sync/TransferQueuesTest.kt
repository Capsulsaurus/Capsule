package com.justin13888.capsule.feature.sync

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ContentHash
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.UploadTier
import com.justin13888.capsule.core.model.asWire
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

/**
 * The derivation behind both queues.
 *
 * Pure, so the two rules that matter most are asserted without a repository anywhere near
 * them: every tier is always listed even when empty, and each tier's gate is the connection
 * class the ladder declares rather than a number invented here.
 */
class TransferQueuesTest {
    @Test
    fun `every rung is listed even when nothing is waiting at it`() {
        val groups = TransferQueues.uploads(listOf(synced()), ConnectionClass.Unmetered)

        assertEquals(UploadTier.entries, groups.map(UploadTierGroup::tier))
        assertTrue(groups.all(UploadTierGroup::isClear))
    }

    @Test
    fun `a metered link opens the index rung and holds the original back`() {
        val groups = TransferQueues.uploads(listOf(queued(UploadTier.Original)), ConnectionClass.Metered)

        assertTrue(groups.first { it.tier == UploadTier.Index }.opensOnThisConnection)
        assertTrue(groups.first { it.tier == UploadTier.Preview }.opensOnThisConnection)
        assertFalse(groups.first { it.tier == UploadTier.Original }.opensOnThisConnection)
    }

    @Test
    fun `an adverse link still opens the index rung, so what exists escapes the device`() {
        val groups = TransferQueues.uploads(listOf(queued(UploadTier.Index)), ConnectionClass.Adverse)

        assertTrue(groups.first { it.tier == UploadTier.Index }.opensOnThisConnection)
        assertEquals(1, groups.first { it.tier == UploadTier.Index }.pending)
    }

    @Test
    fun `offline opens nothing at all`() {
        val groups = TransferQueues.uploads(listOf(queued(UploadTier.Index)), ConnectionClass.Offline)

        assertTrue(groups.none(UploadTierGroup::opensOnThisConnection))
    }

    @Test
    fun `an awaiting-original asset owes exactly its original and pins the local copy`() {
        val groups = TransferQueues.uploads(listOf(awaitingOriginal()), ConnectionClass.Unmetered)
        val original = groups.first { it.tier == UploadTier.Original }

        assertEquals(1, original.pending)
        assertEquals(TierProgress.Queued, original.entries.single().progress)
        assertTrue(original.entries.single().pinsLocalOriginal)
    }

    @Test
    fun `an uploading asset reports how far its session has got`() {
        val groups = TransferQueues.uploads(listOf(uploading(sent = 3, total = 4)), ConnectionClass.Unmetered)
        val entry = groups.first { it.tier == UploadTier.Original }.entries.single()

        assertEquals(TierProgress.Uploading, entry.progress)
        assertEquals(0.75f, entry.fraction, 0f)
    }

    @Test
    fun `a failed rung carries the code the user can act on`() {
        val code = ErrorCode("error.upload.checksum_mismatch")
        val groups = TransferQueues.uploads(listOf(failed(code)), ConnectionClass.Unmetered)

        assertEquals(
            code,
            groups
                .first { it.tier == UploadTier.Original }
                .entries
                .single()
                .failure,
        )
    }

    @Test
    fun `quarantined and local-only assets owe the server nothing`() {
        val groups = TransferQueues.uploads(listOf(localOnly()), ConnectionClass.Unmetered)

        assertTrue(groups.all(UploadTierGroup::isClear))
    }

    @Test
    fun `the fetch queue lists only what the configured scope calls for`() {
        val thumbnailOnly = synced(HeldRepresentations(setOf(RepresentationTier.Lqip, RepresentationTier.Thumbnail)))

        val atThumbnails = TransferQueues.fetches(listOf(thumbnailOnly), SyncScope.MetadataAndThumbnails)
        val atOriginals = TransferQueues.fetches(listOf(thumbnailOnly), SyncScope.MetadataThumbnailsAndOriginal)

        assertTrue(atThumbnails.isEmpty())
        assertEquals(1, atOriginals.size)
        assertEquals(RepresentationTier.Original, atOriginals.single().wanted)
        assertEquals(RepresentationTier.Thumbnail, atOriginals.single().held)
    }

    @Test
    fun `an asset whose original is still on its capturing device reads as degraded`() {
        val entries =
            TransferQueues.fetches(listOf(awaitingOriginal()), SyncScope.MetadataThumbnailsAndOriginal)

        assertTrue(entries.single().isDegraded)
    }

    private fun asset(
        id: String,
        syncState: SyncState,
        held: HeldRepresentations = HeldRepresentations.LQIP_ONLY,
    ) = Asset(
        id = AssetId(id),
        assetType = AssetType.Photo.asWire(),
        captureTimestamp = CAPTURED_AT,
        captureUtc = CAPTURED_AT,
        captureTzSource = null,
        importTimestamp = CAPTURED_AT,
        contentHash = ContentHash("hash-$id"),
        dimensions = null,
        duration = null,
        albumId = null,
        rating = 0,
        isHidden = false,
        heldRepresentations = held,
        syncState = syncState,
    )

    private fun synced(held: HeldRepresentations = LQIP_ONLY) = asset("synced", SyncState.Synced, held)

    private fun localOnly() = asset("local", SyncState.LocalOnly)

    private fun queued(tier: UploadTier) = asset("queued", SyncState.Queued(tier))

    private fun awaitingOriginal() =
        asset(
            "awaiting",
            SyncState.AwaitingOriginal,
            HeldRepresentations(setOf(RepresentationTier.Lqip, RepresentationTier.Thumbnail)),
        )

    private fun uploading(
        sent: Long,
        total: Long,
    ) = asset("uploading", SyncState.Uploading(UploadTier.Original, sent, total))

    private fun failed(code: ErrorCode) = asset("failed", SyncState.Failed(code))

    private companion object {
        val CAPTURED_AT: Instant = Instant.parse("2026-06-01T12:00:00Z")

        /** The ladder's floor, which every synced asset holds by definition. */
        val LQIP_ONLY: HeldRepresentations = HeldRepresentations.LQIP_ONLY
    }
}
