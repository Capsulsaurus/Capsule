package com.justin13888.capsule.core.model

import java.time.Duration
import java.time.Instant

/** Pixel dimensions of a still or a video frame. */
data class Dimensions(
    /** Width in pixels. */
    val width: Int,
    /** Height in pixels. */
    val height: Int,
) {
    /**
     * Width divided by height.
     *
     * The gallery lays out a million assets from this number alone, which is why it is
     * derived here rather than recomputed at every call site — see [AspectRatioCodec] for
     * the byte-per-asset form the layout engine actually consumes.
     */
    val aspectRatio: Float get() = width.toFloat() / height.toFloat()
}

/** A colour as three 8-bit channels — the sidecar's `dominant_color`. */
data class RgbColor(
    /** Red, `0..255`. */
    val red: Int,
    /** Green, `0..255`. */
    val green: Int,
    /** Blue, `0..255`. */
    val blue: Int,
) {
    /** The `#rrggbb` spelling the catalog stores this as. */
    fun toHex(): String = "#%02x%02x%02x".format(red, green, blue)

    companion object {
        /** Parses `#rrggbb` (with or without the leading `#`), or `null` when malformed. */
        fun fromHex(hex: String): RgbColor? {
            val body = hex.removePrefix("#")
            if (body.length != HEX_LENGTH) return null
            val value = body.toIntOrNull(radix = HEX_RADIX) ?: return null
            return RgbColor(
                red = (value shr Short.SIZE_BITS) and BYTE_MASK,
                green = (value shr Byte.SIZE_BITS) and BYTE_MASK,
                blue = value and BYTE_MASK,
            )
        }

        private const val HEX_LENGTH = 6
        private const val HEX_RADIX = 16
        private const val BYTE_MASK = 0xFF
    }
}

/**
 * The low-quality image placeholder carried inside the encrypted metadata blob.
 *
 * Because it rides inside the sidecar rather than being a derivative blob, it is available
 * the instant metadata syncs, at zero extra request — the first rung of the representation
 * ladder costs nothing. It also never reaches the server in the clear.
 *
 * A [formatVersion] the client does not recognise is not an error: the renderer falls back
 * to a 1×1 fill of [dominantColor] rather than misrendering unknown bytes.
 */
data class Lqip(
    /** The encoded Chromahash payload; opaque to this layer. */
    val chromahash: String,
    /** The LQIP encoding version. An unrecognised value degrades to [dominantColor]. */
    val formatVersion: Int,
    /** The single colour to fill with when [chromahash] cannot be decoded. */
    val dominantColor: RgbColor,
)

/**
 * The physical camera that produced an asset — the sidecar's `camera_id`.
 *
 * [serial] is the fingerprinting half: a serial number uniquely links every photo to one
 * device, so it is stripped by default whenever an asset crosses a trust boundary. [make]
 * and [model] identify a product line, not a unit, and survive export.
 */
data class CameraIdentity(
    /** Manufacturer, e.g. `Fujifilm`. */
    val make: String?,
    /** Model name, e.g. `X-T5`. */
    val model: String?,
    /** The unit's serial number — stripped on export unless the user opts in. */
    val serial: String?,
) {
    /** This identity with the fingerprinting [serial] removed, for a boundary crossing. */
    fun redactedForExport(): CameraIdentity = copy(serial = null)
}

/**
 * Where an asset sits in the delete lifecycle.
 *
 * Delete is soft by construction and the retention window is **signed into the delete
 * manifest at delete time**, not read from server config at purge time. Modelling
 * [Trashed.retentionUntil] as data on the state rather than a policy looked up later is the
 * client-side shape of that guarantee: the deadline travels with the deletion, so a server
 * cannot accelerate a purge by changing its own configuration.
 */
sealed interface DeletionState {
    /** Present in the library and in every default view. */
    data object Live : DeletionState

    /**
     * Soft-deleted: still stored, still counted against quota at full size, restorable
     * until [retentionUntil].
     */
    data class Trashed(
        /** When the delete manifest was signed. */
        val deletedAt: Instant,
        /** The signed floor on the purge — a hard purge before this is refused. */
        val retentionUntil: Instant,
    ) : DeletionState {
        /**
         * How long remains before the asset becomes eligible for hard purge, floored at
         * zero once the window has passed.
         */
        fun retentionRemaining(now: Instant): Duration {
            val remaining = Duration.between(now, retentionUntil)
            return if (remaining.isNegative) Duration.ZERO else remaining
        }

        /** Whether the signed window has elapsed and a purge worker may now proceed. */
        fun isPurgeable(now: Instant): Boolean = !now.isBefore(retentionUntil)
    }

    companion object {
        /** The default retention window when the user sets no other. */
        val DEFAULT_RETENTION: Duration = Duration.ofDays(30)
    }
}

/**
 * One imported media file, as the UI needs it.
 *
 * The first block of fields is a field-for-field mirror of the catalog's `AssetRecord`, so
 * a real UniFFI binding maps onto it without reshaping; the rest is projected from the
 * asset's sidecar, which the catalog row does not carry. Nullability follows the record:
 * where the catalog stores an `Option`, so does this.
 *
 * Enum-typed fields are [Wire] rather than bare constants because the catalog hands them
 * over as raw tokens. A value from a newer peer must survive the trip through this type
 * unchanged — stripping it would invalidate the sidecar signature that covers it — while
 * still refusing to drive behaviour it is not understood well enough to drive.
 */
data class Asset(
    /** `AssetRecord.uuid` — the one canonical identity, a UUIDv7 minted at import. */
    val id: AssetId,
    /** `AssetRecord.asset_type`. */
    val assetType: Wire<AssetType>,
    /**
     * `AssetRecord.capture_timestamp` — the capture instant as the device recorded it.
     *
     * This is the timeline's sort key. When [captureTzSource] is
     * [CaptureTzSource.Floating] there is no known offset behind it, so it is a wall-clock
     * reading rather than a true instant; the timeline still sorts by it because the
     * alternative — falling back to import order — puts a scanned photo in the wrong decade.
     */
    val captureTimestamp: Instant,
    /** `AssetRecord.capture_utc` — the true UTC instant, when an offset was established. */
    val captureUtc: Instant?,
    /** `AssetRecord.capture_tz_source` — how that offset was established, if at all. */
    val captureTzSource: Wire<CaptureTzSource>?,
    /** `AssetRecord.import_timestamp`. */
    val importTimestamp: Instant,
    /** `AssetRecord.hash_sha256` — the plaintext digest import deduplicates on. */
    val contentHash: ContentHash,
    /** `AssetRecord.width` / `height`; absent for formats the extractor could not probe. */
    val dimensions: Dimensions?,
    /** `AssetRecord.duration_ms`; absent for stills. */
    val duration: Duration?,
    /** `AssetRecord.album_id` — the one container album this asset lives in. */
    val albumId: AlbumId?,
    /** `AssetRecord.rating` — stars, `0..5`. Orthogonal to [cull]. */
    val rating: Int,
    /**
     * `AssetRecord.is_hidden` — projected from the sidecar's `hidden` LWW register.
     *
     * Excludes the asset from the timeline, search, and every system view; it stays in its
     * container album, keeps syncing, and remains reachable from its stack or an existing
     * share. Hiding is view-layer only, not deletion and not access control.
     */
    val isHidden: Boolean,
    /** Live, or soft-deleted with its signed retention deadline. */
    val deletion: DeletionState = DeletionState.Live,
    /** The stack this asset belongs to, projected from `stack_membership`; `null` if free. */
    val stack: StackMembership? = null,
    /** The precise format, from the sidecar's `content_type`. */
    val contentType: Wire<ContentType>? = null,
    /** The sidecar's `cull` LWW register; [CullFlag.Neutral] when never flagged. */
    val cull: Wire<CullFlag> = CullFlag.Neutral.asWire(),
    /** The placeholder available before any blob is fetched. */
    val lqip: Lqip? = null,
    /** The stored fix, in whatever datum its source supplied. */
    val gps: GpsFix? = null,
    /** The camera that produced the asset. */
    val camera: CameraIdentity? = null,
    /** The importing device — UUIDv4, stripped on export. */
    val importedByDevice: DeviceId? = null,
    /** The session the import ran in — UUIDv7, stripped on export. */
    val importedInSession: SessionId? = null,
    /** Which rungs of the representation ladder this device currently holds. */
    val heldRepresentations: HeldRepresentations = HeldRepresentations.LQIP_ONLY,
    /** Whether the original has landed on the server, and what is still in flight. */
    val syncState: SyncState = SyncState.Synced,
) {
    /**
     * Whether the asset belongs in a default view — the timeline, search results, and every
     * system view.
     *
     * Three independent exclusions collapse here because a caller that forgot one would
     * leak content the user asked to keep out of sight: hidden assets, trashed assets, and
     * the non-primary halves of a collapsed stack (the JPEG of a RAW+JPEG pair, a Live
     * Photo's motion clip), which the catalog projects as `is_stack_hidden`.
     */
    val isInDefaultViews: Boolean
        get() = !isHidden && deletion is DeletionState.Live && stack?.isSuppressed != true

    /** The asset with every fingerprinting field stripped, for a trust-boundary crossing. */
    fun redactedForExport(): Asset =
        copy(
            gps = gps?.redactedForExport(),
            camera = camera?.redactedForExport(),
            importedByDevice = null,
            importedInSession = null,
        )
}
