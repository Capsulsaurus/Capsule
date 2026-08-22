package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DeletionState
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.LifecycleAction
import com.justin13888.capsule.core.model.Lqip
import com.justin13888.capsule.core.model.ModelOutputStatus
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.QuarantineEntry
import com.justin13888.capsule.core.model.QuarantineId
import com.justin13888.capsule.core.model.QuarantineSurface
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.RgbColor
import com.justin13888.capsule.core.model.StackType
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull
import java.time.Duration
import java.time.Instant

/**
 * Forces one asset into each documented state, and records where they went.
 *
 * Left to chance, a generated library would contain a quarantined asset only sometimes and a
 * forward-schema one almost never — and the states that never appear are exactly the ones
 * whose UI nobody ever looks at. Placing them deliberately turns the state matrix from
 * something the fixture might happen to cover into something a test can assert it covers.
 */
internal fun LibraryGenerator.synthesizeStateMatrix(): LibraryLandmarks {
    val pool = assets.filter { it.stack == null && it.duration == null && it.dimensions != null }
    val chosen = spreadPick(pool, LANDMARK_COUNT)
    val hidden = chosen.getOrNull(0)?.id?.also(::markHidden)
    val trashedFresh = chosen.getOrNull(1)?.id?.also { markTrashed(it, Duration.ofHours(FRESH_TRASH_HOURS)) }
    val trashedMid = chosen.getOrNull(2)?.id?.also { markTrashed(it, Duration.ofDays(MID_TRASH_DAYS)) }
    val trashedExpired = chosen.getOrNull(3)?.id?.also { markTrashed(it, Duration.ofDays(EXPIRED_TRASH_DAYS)) }
    val awaiting = chosen.getOrNull(4)?.id?.also(::markAwaitingOriginal)
    val degraded = chosen.getOrNull(5)?.id?.also(::markDegraded)
    val quarantined = chosen.getOrNull(6)?.id?.also(::markQuarantined)
    val forwardSchema = chosen.getOrNull(7)?.id?.also(::markForwardSchema)
    val unreadable = chosen.getOrNull(8)?.id?.also(::markUnreadable)
    populateTrash(pool, chosen)
    hideMore(pool, chosen)
    recordQuarantineSurfaces(quarantined)
    return LibraryLandmarks(
        defaultAlbumId = defaultAlbum.id,
        tripAlbumIds = tripAlbums.values.map { it.id },
        viewAlbumId = albums.first { it.kind.knownOrNull == AlbumKind.View }.id,
        hiddenAssetId = hidden,
        trashedFreshAssetId = trashedFresh,
        trashedMidWindowAssetId = trashedMid,
        trashedExpiredAssetId = trashedExpired,
        awaitingOriginalAssetId = awaiting,
        degradedAssetId = degraded,
        quarantinedAssetId = quarantined,
        forwardSchemaAssetId = forwardSchema,
        unreadableAssetId = unreadable,
        gcj02AssetId = gcj02Asset(),
        rawJpegStackId = stacks.firstOrNull { it.stackType.knownOrNull == StackType.RawJpeg }?.id,
        burstStackId = stacks.firstOrNull { it.stackType.knownOrNull == StackType.Burst }?.id,
        livePhotoStackId = stacks.firstOrNull { it.stackType.knownOrNull == StackType.LivePhoto }?.id,
        chapteredStackId = stacks.firstOrNull { it.stackType.knownOrNull == StackType.Chaptered }?.id,
        panoramaAssetId = widestAsset(),
        videoAssetId = assets.firstOrNull { it.duration != null }?.id,
        squareAssetId = squarestAsset(),
        staleAiTagAssetId = tags.entries.firstOrNull { entry -> entry.value.ai.any(AiTag::isStale) }?.key,
        staleClusterId = clusters.firstOrNull { it.status.knownOrNull == ModelOutputStatus.Stale }?.id,
        thisDeviceId = thisDevice.id,
        ownerId = owner,
    )
}

/** Distinct assets taken at even intervals through [pool], so the states are spread out. */
private fun spreadPick(
    pool: List<Asset>,
    count: Int,
): List<Asset> {
    if (pool.isEmpty()) return emptyList()
    return (0 until count).mapNotNull { index -> pool.getOrNull(pool.size * (index + 1) / (count + 2)) }.distinct()
}

/** Hidden: still in its album, still syncing, absent from every default view. */
private fun LibraryGenerator.markHidden(id: AssetId) = mutateAsset(id) { it.copy(isHidden = true) }

/**
 * Soft-deleted [deletedAgo] ago, with the retention window signed in at delete time.
 *
 * `retention_until` is derived from the delete instant rather than looked up now, which is the
 * client-side shape of the guarantee: the deadline travels with the deletion, so nothing
 * downstream can shorten it by changing its own configuration.
 */
private fun LibraryGenerator.markTrashed(
    id: AssetId,
    deletedAgo: Duration,
) {
    val deletedAt = spec.now.minus(deletedAgo)
    val retentionUntil = deletedAt.plus(DeletionState.DEFAULT_RETENTION)
    mutateAsset(id) { it.copy(deletion = DeletionState.Trashed(deletedAt, retentionUntil)) }
    recordDeleteChain(id, deletedAt, retentionUntil)
}

/** Visible everywhere, but its original has not left the capturing device yet. */
private fun LibraryGenerator.markAwaitingOriginal(id: AssetId) =
    mutateAsset(id) {
        it.copy(
            syncState = SyncState.AwaitingOriginal,
            heldRepresentations = HeldRepresentations(setOf(RepresentationTier.Lqip, RepresentationTier.Thumbnail)),
        )
    }

/** Holds only the placeholder, so every above-tier fetch has somewhere to degrade to. */
private fun LibraryGenerator.markDegraded(id: AssetId) {
    mutateAsset(id) { it.copy(heldRepresentations = HeldRepresentations.LQIP_ONLY) }
}

/** Inbound state was refused; the asset waits for a human rather than being applied or dropped. */
private fun LibraryGenerator.markQuarantined(id: AssetId) =
    mutateAsset(id) { it.copy(syncState = SyncState.Quarantined(QuarantineSurface.StaleRevival)) }

/**
 * Written by a peer running a later `protocol_version`.
 *
 * The unknown tokens are preserved verbatim rather than normalised away, because stripping a
 * field a newer peer wrote would invalidate the sidecar signature that covers it. The client
 * must be able to carry this asset through a sync round-trip while refusing to let the
 * unknown values drive any behaviour.
 */
private fun LibraryGenerator.markForwardSchema(id: AssetId) =
    mutateAsset(id) {
        it.copy(
            assetType = Wire.Unknown("volumetric"),
            cull = Wire.Unknown("shortlist"),
            captureTzSource = Wire.Unknown("network_time"),
        )
    }

/**
 * A format no decoder on this device claims.
 *
 * Its dimensions are absent because the extractor could not probe the file at all, which is
 * what sends the layout pass down the [com.justin13888.capsule.core.model.AspectRatioCodec]
 * square fallback, and its placeholder carries an unrecognised format version so the renderer
 * has to fall back to the dominant colour.
 */
private fun LibraryGenerator.markUnreadable(id: AssetId) =
    mutateAsset(id) {
        it.copy(
            contentType = Wire.Unknown("image/x-vendor-raw"),
            dimensions = null,
            lqip =
                Lqip(
                    chromahash = "",
                    formatVersion = UNKNOWN_LQIP_VERSION,
                    dominantColor = RgbColor(GREY_LEVEL, GREY_LEVEL, GREY_LEVEL),
                ),
            heldRepresentations = HeldRepresentations.LQIP_ONLY,
        )
    }

/** Enough trashed assets that the Recently Deleted view is worth looking at. */
private fun LibraryGenerator.populateTrash(
    pool: List<Asset>,
    already: List<Asset>,
) {
    val prng = metadataPrng.derive("trash")
    pool
        .filterIndexed { index, asset -> index % TRASH_STRIDE == 0 && asset !in already }
        .take(EXTRA_TRASHED)
        .forEach { asset -> markTrashed(asset.id, Duration.ofDays(prng.nextInt(1, MID_TRASH_DAYS.toInt()).toLong())) }
}

/** A handful more hidden assets, so the gated Hidden view is not a one-row list. */
private fun LibraryGenerator.hideMore(
    pool: List<Asset>,
    already: List<Asset>,
) {
    pool
        .filterIndexed { index, asset -> index % HIDDEN_STRIDE == 0 && asset !in already }
        .take(EXTRA_HIDDEN)
        .forEach { asset -> markHidden(asset.id) }
}

/** The chain a delete appends — the create record stays, so the history stays legible. */
private fun LibraryGenerator.recordDeleteChain(
    id: AssetId,
    deletedAt: Instant,
    retentionUntil: Instant,
) {
    val asset = assetOf(id) ?: return
    val createHash = DeterministicIds.hexDigest(idPrng)
    provenance[id] =
        listOf(
            ProvenanceRecord(
                assetId = id,
                action = LifecycleAction.Create.asWire(),
                priorProvenanceHash = null,
                recordHash = createHash,
                byDevice = thisDevice.id,
                at = asset.importTimestamp,
            ),
            ProvenanceRecord(
                assetId = id,
                action = LifecycleAction.Delete.asWire(),
                // Naming the chain head it followed is what makes a replayed stale manifest
                // detectable: a peer proposing a write behind this point is quarantined.
                priorProvenanceHash = createHash,
                recordHash = DeterministicIds.hexDigest(idPrng),
                byDevice = thisDevice.id,
                at = deletedAt,
                retentionUntil = retentionUntil,
            ),
        )
}

/** The surfaces holding something for a human decision. */
private fun LibraryGenerator.recordQuarantineSurfaces(quarantinedAssetId: AssetId?) {
    val detectedAt = spec.now.minus(Duration.ofHours(QUARANTINE_HOURS_AGO))
    if (quarantinedAssetId != null) {
        quarantine +=
            QuarantineEntry(
                id = QuarantineId(DeterministicIds.uuidV7(detectedAt.toEpochMilli(), idPrng)),
                surface = QuarantineSurface.StaleRevival.asWire(),
                assetId = quarantinedAssetId,
                detectedAt = detectedAt,
                detail = "A peer proposed a manifest naming a chain head this device has already moved past.",
                code = ErrorCode("error.upload.stale_revival"),
            )
    }
    quarantine +=
        QuarantineEntry(
            id = QuarantineId(DeterministicIds.uuidV7(detectedAt.toEpochMilli(), idPrng)),
            surface = QuarantineSurface.MalformedSidecar.asWire(),
            assetId = null,
            detectedAt = detectedAt.minus(Duration.ofDays(1)),
            detail = "A sidecar's CBOR would not parse; the unreadable bytes are preserved as-is.",
        )
    drops.firstOrNull()?.let { drop ->
        quarantine +=
            QuarantineEntry(
                id = QuarantineId(DeterministicIds.uuidV7(drop.receivedAt.toEpochMilli(), idPrng)),
                surface = QuarantineSurface.PendingDropReview.asWire(),
                assetId = null,
                detectedAt = drop.receivedAt,
                detail = "A guest deposited a file through an upload link and it awaits review.",
            )
    }
}

/**
 * The asset whose stored fix is in GCJ-02.
 *
 * Looked up rather than forced when the Shanghai route made it into the plan, and forced onto
 * an ordinary asset when it did not — a shorter span may not reach that trip, and the datum
 * case has to be present in every fixture regardless.
 */
private fun LibraryGenerator.gcj02Asset(): AssetId? {
    assets.firstOrNull { it.gps?.datum?.knownOrNull == GpsDatum.Gcj02 }?.let { return it.id }
    val candidate = assets.firstOrNull { it.gps != null } ?: return null
    mutateAsset(candidate.id) {
        it.copy(gps = GeoRoutes.SHANGHAI_BUND.sample(SHANGHAI_FRACTION, GCJ_JITTER_METRES, gpsPrng))
    }
    return candidate.id
}

/** The widest frame in the library — the panorama a layout pass has to clamp. */
private fun LibraryGenerator.widestAsset(): AssetId? =
    assets
        .filter { it.dimensions != null }
        .maxByOrNull { it.dimensions?.aspectRatio ?: 0f }
        ?.id

/** The frame closest to square. */
private fun LibraryGenerator.squarestAsset(): AssetId? =
    assets
        .filter { it.dimensions != null }
        .minByOrNull { kotlin.math.abs((it.dimensions?.aspectRatio ?: 0f) - 1f) }
        ?.id

/** How many landmark slots the state matrix consumes. */
private const val LANDMARK_COUNT = 9
private const val FRESH_TRASH_HOURS = 6L
private const val MID_TRASH_DAYS = 16L
private const val EXPIRED_TRASH_DAYS = 41L
private const val TRASH_STRIDE = 211
private const val EXTRA_TRASHED = 34
private const val HIDDEN_STRIDE = 307
private const val EXTRA_HIDDEN = 8
private const val UNKNOWN_LQIP_VERSION = 99
private const val GREY_LEVEL = 122
private const val QUARANTINE_HOURS_AGO = 20L
private const val SHANGHAI_FRACTION = 0.4
private const val GCJ_JITTER_METRES = 30.0
