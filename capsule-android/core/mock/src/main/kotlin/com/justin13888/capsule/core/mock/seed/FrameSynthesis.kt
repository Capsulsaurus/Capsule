package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.mock.image.ChromaGrid
import com.justin13888.capsule.core.mock.image.ChromaHash
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.CameraIdentity
import com.justin13888.capsule.core.model.CaptureTzSource
import com.justin13888.capsule.core.model.ContentHash
import com.justin13888.capsule.core.model.ContentType
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.Dimensions
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.Lqip
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.RgbColor
import com.justin13888.capsule.core.model.StackMembership
import com.justin13888.capsule.core.model.SyncState
import com.justin13888.capsule.core.model.UploadTier
import com.justin13888.capsule.core.model.asWire
import java.time.Duration
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneOffset

/**
 * Everything one frame needs, gathered so the asset builder takes a single argument.
 *
 * A record rather than a long parameter list: the same fifteen values are assembled by the
 * single-frame path and by each of the four stack shapes, and threading them positionally
 * through five call sites is how a burst ends up filed in the wrong album.
 */
internal data class FrameSpec(
    /** The day this frame was shot. */
    val day: PlannedDay,
    /** The local wall-clock reading the camera recorded. */
    val localTime: LocalDateTime,
    /** The body it was shot on. */
    val camera: CameraProfile,
    /** The lighting situation the exposure was chosen for. */
    val scene: CaptureScene,
    /** The precise format. */
    val contentType: ContentType,
    /** Width ÷ height. */
    val aspectRatio: Double,
    /** The container it was filed into. */
    val albumId: AlbumId?,
    /** Runtime, for a video. */
    val duration: Duration? = null,
    /** Whether a fix was recorded at all. */
    val hasGps: Boolean = true,
    /** How the capture offset was established. */
    val tzSource: CaptureTzSource = CaptureTzSource.OffsetExif,
    /** The stack this frame belongs to, when it belongs to one. */
    val membership: StackMembership? = null,
    /** How far along the day's route the frame was taken, `0.0..1.0`. */
    val routeFraction: Double = 0.0,
    /** The culling decision, for a shoot that has been reviewed. */
    val cull: CullFlag = CullFlag.Neutral,
    /** Stars. */
    val rating: Int = 0,
)

/** The shape a moment in a day takes. */
internal enum class FrameShape {
    /** A screenshot: no optics, no coordinates. */
    Screenshot,

    /** A RAW and its processed JPEG. */
    RawJpegPair,

    /** A high-speed sequence with one chosen keeper. */
    Burst,

    /** A still and its motion clip. */
    LivePhoto,

    /** One long recording split at a file-size limit. */
    ChapteredVideo,

    /** A free-standing clip. */
    SingleVideo,

    /** An ordinary still. */
    SingleFrame,
}

/**
 * Fills one planned day with frames.
 *
 * Frames are laid down along a moving time cursor rather than at independently drawn instants
 * that are sorted afterwards. The cursor is what makes a burst possible at all: its members
 * are a hundred milliseconds apart, which a "draw N times and sort" scheme cannot express
 * without special-casing it back in.
 */
internal fun LibraryGenerator.synthesizeDay(day: PlannedDay) {
    if (day.assetCount == 0) return
    val window = activeWindow(day.kind)
    val kit = kitFor(day)
    val album = albumFor(day)
    val windowSeconds = (window.second.toSecondOfDay() - window.first.toSecondOfDay()).coerceAtLeast(1)
    val step = (windowSeconds.toDouble() / day.assetCount).coerceAtLeast(1.0)
    var cursor = window.first.toSecondOfDay() + framePrng.nextInt(0, START_JITTER_SECONDS)
    var remaining = day.assetCount
    while (remaining > 0) {
        val camera = framePrng.pickWeighted(kit, kit.map(CameraProfile::weight))
        val at = day.date.atTime(LocalTime.ofSecondOfDay(cursor.toLong().coerceIn(0, LAST_SECOND_OF_DAY)))
        val fraction = 1.0 - remaining.toDouble() / day.assetCount
        val emitted = emitFrames(day, camera, at, album, fraction, remaining)
        remaining -= emitted
        cursor += (step * emitted * framePrng.nextDouble(GAP_JITTER_MIN, GAP_JITTER_MAX)).toInt()
    }
}

/**
 * Emits whatever [chooseShape] picked, and reports how many assets it produced.
 *
 * The decision and the emission are separate so neither grows a branch that belongs to the
 * other: the choice is about probability and how many frames are left, the emission is about
 * what a stack of that kind looks like, and a single function doing both is where a "burst of
 * one frame" bug comes from.
 */
private fun LibraryGenerator.emitFrames(
    day: PlannedDay,
    camera: CameraProfile,
    at: LocalDateTime,
    album: AlbumId?,
    fraction: Double,
    remaining: Int,
): Int {
    val context = GroupContext(day, camera, at, album, fraction)
    return when (chooseShape(day, camera, remaining)) {
        FrameShape.Screenshot -> emitScreenshot(context)
        FrameShape.RawJpegPair -> emitRawJpegPair(context)
        FrameShape.Burst -> emitBurst(context, remaining)
        FrameShape.LivePhoto -> emitLivePhoto(context)
        FrameShape.ChapteredVideo -> emitChapteredVideo(context, remaining)
        FrameShape.SingleVideo -> emitSingleVideo(context)
        FrameShape.SingleFrame -> emitSingleFrame(context)
    }
}

/** One ordinary still. */
private fun LibraryGenerator.emitSingleFrame(context: GroupContext): Int {
    val scene = drawScene(context.day, context.at)
    addAsset(
        makeAsset(
            FrameSpec(
                day = context.day,
                localTime = context.at,
                camera = context.camera,
                scene = scene,
                contentType = stillContentType(context.camera),
                aspectRatio = drawAspect(context.camera),
                albumId = context.album,
                routeFraction = context.fraction,
                cull = drawCull(context.day),
                rating = drawRating(context.day),
            ),
        ),
    )
    return 1
}

/** One free-standing clip, not part of a chaptered recording. */
private fun LibraryGenerator.emitSingleVideo(context: GroupContext): Int {
    addAsset(
        makeAsset(
            FrameSpec(
                day = context.day,
                localTime = context.at,
                camera = context.camera,
                scene = drawScene(context.day, context.at),
                contentType = videoContentType(context.camera),
                aspectRatio = framePrng.pickWeighted(VIDEO_ASPECTS, VIDEO_ASPECT_WEIGHTS),
                albumId = context.album,
                duration = Duration.ofSeconds(framePrng.nextInt(CLIP_SECONDS_MIN, CLIP_SECONDS_MAX).toLong()),
                routeFraction = context.fraction,
            ),
        ),
    )
    return 1
}

/** A screenshot: no optics, no coordinates, and the phone's own display aspect. */
private fun LibraryGenerator.emitScreenshot(context: GroupContext): Int {
    addAsset(
        makeAsset(
            FrameSpec(
                day = context.day,
                localTime = context.at,
                camera = CameraBag.IPHONE_15_PRO,
                scene = CaptureScene.ScreenCapture,
                contentType = ContentType.Png,
                aspectRatio = SCREENSHOT_ASPECT,
                albumId = context.album,
                hasGps = false,
                tzSource = CaptureTzSource.OffsetExif,
                routeFraction = context.fraction,
            ),
        ),
    )
    return 1
}

/** Builds the asset [spec] describes and records its EXIF beside it. */
internal fun LibraryGenerator.makeAsset(spec: FrameSpec): Asset {
    val offsetSeconds = spec.day.route.offsetSeconds
    val floating = spec.tzSource == CaptureTzSource.Floating
    // The wall-clock reading the device recorded, read as-is; the true instant is only known
    // when an offset was established, which is exactly what `capture_tz_source` records.
    val captureTimestamp = spec.localTime.toInstant(ZoneOffset.UTC)
    val captureUtc = if (floating) null else spec.localTime.toInstant(ZoneOffset.ofTotalSeconds(offsetSeconds))
    val id = AssetId(DeterministicIds.uuidV7(captureTimestamp.toEpochMilli(), idPrng))
    val dimensions = dimensionsFor(spec)
    val asset =
        Asset(
            id = id,
            assetType = (if (spec.duration != null) AssetType.Video else AssetType.Photo).asWire(),
            captureTimestamp = captureTimestamp,
            captureUtc = captureUtc,
            captureTzSource = spec.tzSource.asWire(),
            importTimestamp = (captureUtc ?: captureTimestamp).plus(importDelay()),
            contentHash = ContentHash(DeterministicIds.hexDigest(idPrng)),
            dimensions = dimensions,
            duration = spec.duration,
            albumId = spec.albumId,
            rating = spec.rating,
            isHidden = false,
            stack = spec.membership,
            contentType = spec.contentType.asWire(),
            cull = spec.cull.asWire(),
            lqip = lqipFor(spec),
            gps = gpsFor(spec),
            camera = cameraIdentityOf(spec.camera),
            importedByDevice = thisDevice.id,
            importedInSession = importSession,
            heldRepresentations = heldFor(captureTimestamp),
            syncState = syncStateFor(captureTimestamp),
        )
    exif[id] =
        ExifSynthesis.synthesize(
            camera = spec.camera,
            scene = spec.scene,
            zoneId = if (floating) null else spec.day.route.timeZoneId,
            offsetSeconds = if (floating) null else offsetSeconds,
            prng = exifPrng,
        )
    return asset
}

/** The hours of the day a person of this kind of day was actually shooting. */
private fun activeWindow(kind: DayKind): Pair<LocalTime, LocalTime> =
    when (kind) {
        DayKind.Trip -> LocalTime.of(6, 0) to LocalTime.of(23, 0)
        DayKind.StudioShoot -> LocalTime.of(10, 0) to LocalTime.of(18, 0)
        DayKind.Weekend -> LocalTime.of(9, 0) to LocalTime.of(23, 30)
        else -> LocalTime.of(8, 0) to LocalTime.of(22, 30)
    }

/** The bodies carried on a day of this kind. */
private fun kitFor(day: PlannedDay): List<CameraProfile> =
    when (day.kind) {
        DayKind.Trip -> CameraBag.TRAVEL_KIT
        DayKind.StudioShoot -> CameraBag.STUDIO_KIT
        DayKind.ScreenshotRun -> listOf(CameraBag.IPHONE_15_PRO)
        else -> CameraBag.ALL
    }

/** Where a day's frames are filed. */
private fun LibraryGenerator.albumFor(day: PlannedDay): AlbumId? =
    when {
        day.trip != null -> tripAlbums[day.trip.index]?.id ?: defaultAlbum.id
        day.kind == DayKind.StudioShoot -> albums.first { it.name == "Studio — Client Work" }.id
        day.kind == DayKind.Weekend && framePrng.chance(FAMILY_ALBUM_SHARE) -> albums.first { it.name == "Family" }.id
        else -> defaultAlbum.id
    }

/** Pixel dimensions from the body's sensor and the frame's ratio, rounded to even edges. */
private fun LibraryGenerator.dimensionsFor(spec: FrameSpec): Dimensions {
    val longEdge = framePrng.pick(spec.camera.sensorLongEdges)
    val capped = if (spec.duration != null) longEdge.coerceAtMost(VIDEO_LONG_EDGE_CAP) else longEdge
    val width = if (spec.aspectRatio >= 1.0) capped else (capped * spec.aspectRatio).toInt()
    val height = if (spec.aspectRatio >= 1.0) (capped / spec.aspectRatio).toInt() else capped
    return Dimensions(width = (width / 2) * 2, height = (height / 2) * 2)
}

/** The still format this body writes. */
private fun LibraryGenerator.stillContentType(camera: CameraProfile): ContentType =
    camera.contentTypes.firstOrNull { it != ContentType.Dng } ?: ContentType.Jpeg

/** The video container this body writes. */
private fun videoContentType(camera: CameraProfile): ContentType =
    if (camera === CameraBag.IPHONE_15_PRO) ContentType.QuickTime else ContentType.Mp4

/** The placeholder that rides inside the encrypted metadata blob. */
private fun LibraryGenerator.lqipFor(spec: FrameSpec): Lqip {
    val palette = PALETTES[spec.scene] ?: PALETTES.getValue(CaptureScene.Daylight)
    val cells =
        List(ChromaGrid.CELL_COUNT) { cell ->
            val base = palette[cell % palette.size]
            RgbColor(
                red = (base.red + lqipPrng.nextInt(-CHROMA_JITTER, CHROMA_JITTER)).coerceIn(0, CHANNEL_MAX),
                green = (base.green + lqipPrng.nextInt(-CHROMA_JITTER, CHROMA_JITTER)).coerceIn(0, CHANNEL_MAX),
                blue = (base.blue + lqipPrng.nextInt(-CHROMA_JITTER, CHROMA_JITTER)).coerceIn(0, CHANNEL_MAX),
            )
        }
    val grid = ChromaGrid(cells)
    return Lqip(
        chromahash = ChromaHash.encode(grid),
        formatVersion = ChromaHash.CURRENT_VERSION,
        dominantColor = grid.mean(),
    )
}

/** The stored fix, in whatever datum the day's route supplies. */
private fun LibraryGenerator.gpsFor(spec: FrameSpec): GpsFix? =
    if (!spec.hasGps) null else spec.day.route.sample(spec.routeFraction, GPS_JITTER_METRES, gpsPrng)

/** The camera identity, serial included — it is stripped on export, not at rest. */
private fun cameraIdentityOf(camera: CameraProfile): CameraIdentity =
    CameraIdentity(make = camera.make, model = camera.model, serial = SERIALS.getValue(camera.model))

/**
 * Which rungs of the ladder this device holds.
 *
 * Recent assets hold a preview because the user has been looking at them; older ones hold
 * only the grid thumbnail, which is what a real cache under pressure looks like after a few
 * months of scrolling.
 */
private fun LibraryGenerator.heldFor(captureTimestamp: java.time.Instant): HeldRepresentations {
    val ageDays = Duration.between(captureTimestamp, spec.now).toDays()
    return when {
        ageDays <= PREVIEW_CACHE_DAYS ->
            HeldRepresentations(
                setOf(RepresentationTier.Lqip, RepresentationTier.Thumbnail, RepresentationTier.Preview),
            )

        else -> HeldRepresentations(setOf(RepresentationTier.Lqip, RepresentationTier.Thumbnail))
    }
}

/** Where the asset stands relative to the server — only the newest frames are still moving. */
private fun LibraryGenerator.syncStateFor(captureTimestamp: java.time.Instant): SyncState {
    val ageHours = Duration.between(captureTimestamp, spec.now).toHours()
    return when {
        ageHours > IN_FLIGHT_HOURS -> SyncState.Synced
        framePrng.chance(QUEUED_SHARE) -> SyncState.Queued(UploadTier.Original)
        else ->
            SyncState.Uploading(
                tier = UploadTier.Original,
                bytesSent = framePrng.nextInt(1, UPLOAD_TOTAL_KB).toLong() * BYTES_PER_KB,
                bytesTotal = UPLOAD_TOTAL_KB.toLong() * BYTES_PER_KB,
            )
    }
}

/** How long after capture the file reached the library. */
private fun LibraryGenerator.importDelay(): Duration =
    Duration.ofMinutes(framePrng.nextInt(IMPORT_DELAY_MIN_MINUTES, IMPORT_DELAY_MAX_MINUTES).toLong())

/** The shared arguments of one emitted group, so each shape takes a single parameter. */
internal data class GroupContext(
    /** The day being filled. */
    val day: PlannedDay,
    /** The body in hand. */
    val camera: CameraProfile,
    /** The wall-clock reading the first member carries. */
    val at: LocalDateTime,
    /** The container the group is filed into. */
    val album: AlbumId?,
    /** How far along the day's route the group sits. */
    val fraction: Double,
)

private const val START_JITTER_SECONDS = 900
private const val LAST_SECOND_OF_DAY = 86_399L
private const val GAP_JITTER_MIN = 0.45
private const val GAP_JITTER_MAX = 1.75
private const val FAMILY_ALBUM_SHARE = 0.35
private const val CLIP_SECONDS_MIN = 5
private const val CLIP_SECONDS_MAX = 180
private const val SCREENSHOT_ASPECT = 1170.0 / 2532.0
private const val VIDEO_LONG_EDGE_CAP = 3840
private const val GPS_JITTER_METRES = 45.0
private const val CHROMA_JITTER = 26
private const val CHANNEL_MAX = 255
private const val PREVIEW_CACHE_DAYS = 60L
private const val IN_FLIGHT_HOURS = 30L
private const val QUEUED_SHARE = 0.5
private const val UPLOAD_TOTAL_KB = 12_000
private const val BYTES_PER_KB = 1024L
private const val IMPORT_DELAY_MIN_MINUTES = 2
private const val IMPORT_DELAY_MAX_MINUTES = 4_000

/** Video is shot landscape far more often than portrait, but both happen. */
private val VIDEO_ASPECTS = listOf(16.0 / 9, 9.0 / 16, 4.0 / 3)

private val VIDEO_ASPECT_WEIGHTS = listOf(66.0, 26.0, 8.0)

/** Serial numbers per body — the fingerprinting field that export strips. */
private val SERIALS =
    mapOf(
        "X-T5" to "2CQ04837",
        "ILCE-7RM5" to "3852119",
        "EOS R5" to "013021000634",
        "iPhone 15 Pro" to "F2LX4K9PQ1",
        "Mini 4 Pro" to "4B7RH2N0090K3M",
        "OpticFilm 8200i" to "OF82-114-772",
    )

/** A three-colour palette per scene, which is what makes a placeholder grid look like a photo. */
private val PALETTES: Map<CaptureScene, List<RgbColor>> =
    mapOf(
        CaptureScene.Daylight to listOf(RgbColor(118, 158, 196), RgbColor(196, 200, 178), RgbColor(84, 106, 72)),
        CaptureScene.GoldenHour to listOf(RgbColor(226, 156, 84), RgbColor(180, 106, 78), RgbColor(96, 70, 74)),
        CaptureScene.IndoorAmbient to listOf(RgbColor(158, 140, 118), RgbColor(112, 96, 84), RgbColor(74, 66, 60)),
        CaptureScene.NightCity to listOf(RgbColor(30, 34, 56), RgbColor(72, 60, 96), RgbColor(190, 148, 92)),
        CaptureScene.Action to listOf(RgbColor(96, 128, 150), RgbColor(168, 172, 160), RgbColor(60, 72, 66)),
        CaptureScene.Macro to listOf(RgbColor(120, 148, 96), RgbColor(196, 188, 140), RgbColor(70, 82, 58)),
        CaptureScene.ScreenCapture to listOf(RgbColor(242, 242, 246), RgbColor(210, 214, 224), RgbColor(60, 62, 70)),
    )
