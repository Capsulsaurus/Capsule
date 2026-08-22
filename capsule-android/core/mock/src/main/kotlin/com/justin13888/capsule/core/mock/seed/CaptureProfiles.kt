package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.ContentType
import java.util.Locale

/**
 * A lighting situation, and the exposure envelope a competent photographer would use in it.
 *
 * Scenes exist so that ISO, shutter and aperture agree with each other and with the picture.
 * Drawing the three independently is the tell that gives a fake library away instantly: it
 * produces ISO 6400 at f/16 and 1/2000s in daylight, a combination no camera would ever
 * record, and any EXIF panel in the app then renders visible nonsense.
 */
enum class CaptureScene(
    /** The ISO band a body would land in here. */
    val isoRange: IntRange,
    /** The shutter denominator band — `1/x` seconds. */
    val shutterDenominatorRange: IntRange,
    /** The aperture band, as f-numbers. */
    val apertureRange: ClosedFloatingPointRange<Double>,
    /** How often this scene comes up, relative to the others. */
    val weight: Double,
) {
    /** Open shade or overcast midday — the bulk of any library. */
    Daylight(
        isoRange = 100..400,
        shutterDenominatorRange = 250..2000,
        apertureRange = 4.0..11.0,
        weight = 34.0,
    ),

    /** The hour after sunrise and before sunset. */
    GoldenHour(
        isoRange = 160..800,
        shutterDenominatorRange = 125..800,
        apertureRange = 1.8..5.6,
        weight = 14.0,
    ),

    /** Indoors under mixed artificial light. */
    IndoorAmbient(
        isoRange = 800..3200,
        shutterDenominatorRange = 60..160,
        apertureRange = 1.4..2.8,
        weight = 20.0,
    ),

    /** A lit street after dark. */
    NightCity(
        isoRange = 1600..12800,
        shutterDenominatorRange = 15..80,
        apertureRange = 1.4..2.8,
        weight = 8.0,
    ),

    /** Sport, wildlife, children — shutter first, everything else follows. */
    Action(
        isoRange = 400..3200,
        shutterDenominatorRange = 1000..4000,
        apertureRange = 2.8..5.6,
        weight = 10.0,
    ),

    /** Close focus, stopped down for depth of field. */
    Macro(
        isoRange = 200..1600,
        shutterDenominatorRange = 125..500,
        apertureRange = 8.0..16.0,
        weight = 5.0,
    ),

    /** A screenshot or a screen recording — no optics involved at all. */
    ScreenCapture(
        isoRange = 0..0,
        shutterDenominatorRange = 0..0,
        apertureRange = 0.0..0.0,
        weight = 9.0,
    ),
}

/**
 * A lens, with the bands it can actually reach.
 *
 * Held beside its body rather than in a global pool so a Fujifilm body never reports a Sony
 * G Master: a mismatched body-and-lens pair is the second tell, after mismatched exposure,
 * that a viewer's metadata panel is showing invented data.
 */
data class LensProfile(
    /** The lens name as EXIF spells it. */
    val model: String,
    /** The focal lengths this lens offers, in millimetres. */
    val focalLengthsMm: List<Int>,
    /** The widest aperture it opens to. */
    val maxAperture: Double,
)

/**
 * A camera body, the lenses that go on it, and what it writes to a file.
 *
 * [contentTypes] is part of the body rather than drawn separately because the format is a
 * property of the camera: an iPhone writes HEIC and a mirrorless body shooting RAW+JPEG
 * writes DNG beside JPEG. Drawing the format independently would produce a phone emitting
 * Adobe DNG, which no phone in this list does.
 */
data class CameraProfile(
    /** EXIF `Make`. */
    val make: String,
    /** EXIF `Model`. */
    val model: String,
    /** The lenses mounted on it in this library. */
    val lenses: List<LensProfile>,
    /** The still formats it writes. */
    val contentTypes: List<ContentType>,
    /** The still resolutions it produces, long edge first. */
    val sensorLongEdges: List<Int>,
    /** Whether this body shoots RAW beside its JPEG, which is what makes a RAW+JPEG stack. */
    val shootsRaw: Boolean,
    /** How often this body appears in the library, relative to the others. */
    val weight: Double,
)

/**
 * The camera bag behind the generated library — one working professional's kit plus the
 * phone in their pocket.
 *
 * Deliberately small and lopsided. A real library is not an even spread across six bodies:
 * it is dominated by whichever camera is always to hand, with the heavy bodies appearing in
 * concentrated runs around the shoots they were carried on.
 */
object CameraBag {
    /** The Fujifilm body most of the personal work is shot on. */
    val FUJIFILM_XT5 =
        CameraProfile(
            make = "Fujifilm",
            model = "X-T5",
            lenses =
                listOf(
                    LensProfile("XF23mmF1.4 R LM WR", listOf(23), 1.4),
                    LensProfile("XF56mmF1.2 R", listOf(56), 1.2),
                    LensProfile("XF16-55mmF2.8 R LM WR", listOf(16, 23, 35, 55), 2.8),
                ),
            contentTypes = listOf(ContentType.Jpeg, ContentType.Dng),
            sensorLongEdges = listOf(7728),
            shootsRaw = true,
            weight = 22.0,
        )

    /** The Sony body the paid work is shot on. */
    val SONY_A7R5 =
        CameraProfile(
            make = "Sony",
            model = "ILCE-7RM5",
            lenses =
                listOf(
                    LensProfile("FE 24-70mm F2.8 GM II", listOf(24, 35, 50, 70), 2.8),
                    LensProfile("FE 85mm F1.4 GM", listOf(85), 1.4),
                ),
            contentTypes = listOf(ContentType.Jpeg, ContentType.Dng),
            sensorLongEdges = listOf(9504),
            shootsRaw = true,
            weight = 14.0,
        )

    /** The Canon body borrowed for long-lens work. */
    val CANON_R5 =
        CameraProfile(
            make = "Canon",
            model = "EOS R5",
            lenses = listOf(LensProfile("RF70-200mm F2.8 L IS USM", listOf(70, 100, 135, 200), 2.8)),
            contentTypes = listOf(ContentType.Jpeg),
            sensorLongEdges = listOf(8192),
            shootsRaw = false,
            weight = 6.0,
        )

    /** The phone, and therefore the body most frames actually come from. */
    val IPHONE_15_PRO =
        CameraProfile(
            make = "Apple",
            model = "iPhone 15 Pro",
            lenses =
                listOf(
                    LensProfile("iPhone 15 Pro back triple camera 6.86mm f/1.78", listOf(13, 24, 77), 1.78),
                ),
            contentTypes = listOf(ContentType.Heic, ContentType.Jpeg),
            sensorLongEdges = listOf(8064, 4032),
            shootsRaw = false,
            weight = 46.0,
        )

    /** The drone, which shows up only on trips. */
    val DJI_MINI_4_PRO =
        CameraProfile(
            make = "DJI",
            model = "Mini 4 Pro",
            lenses = listOf(LensProfile("FC8482", listOf(24), 1.7)),
            contentTypes = listOf(ContentType.Jpeg, ContentType.Dng),
            sensorLongEdges = listOf(8064),
            shootsRaw = true,
            weight = 4.0,
        )

    /** A scanner, so the library has something with no usable capture offset at all. */
    val FILM_SCANNER =
        CameraProfile(
            make = "Plustek",
            model = "OpticFilm 8200i",
            lenses = listOf(LensProfile("", emptyList(), 0.0)),
            contentTypes = listOf(ContentType.Tiff),
            sensorLongEdges = listOf(7200),
            shootsRaw = false,
            weight = 1.0,
        )

    /** Every body, in a fixed order so a weighted draw is reproducible. */
    val ALL: List<CameraProfile> =
        listOf(FUJIFILM_XT5, SONY_A7R5, CANON_R5, IPHONE_15_PRO, DJI_MINI_4_PRO, FILM_SCANNER)

    /** The bodies carried on a trip — the phone plus whatever was worth the weight. */
    val TRAVEL_KIT: List<CameraProfile> = listOf(IPHONE_15_PRO, FUJIFILM_XT5, DJI_MINI_4_PRO)

    /** The bodies a paid shoot is worked on. */
    val STUDIO_KIT: List<CameraProfile> = listOf(SONY_A7R5, CANON_R5)
}

/**
 * The EXIF an asset would carry, kept beside the asset rather than inside it.
 *
 * [com.justin13888.capsule.core.model.Asset] deliberately carries only the camera identity —
 * the rest of an image's EXIF lives in the encrypted metadata blob, which the catalog row
 * does not project. Modelling it here rather than widening `Asset` keeps the domain type
 * honest about what a catalog query actually returns, while still giving a viewer's
 * information panel something real to render.
 */
data class MockExif(
    /** EXIF `Make`. */
    val make: String,
    /** EXIF `Model`. */
    val model: String,
    /** EXIF `LensModel`; empty when no optics were involved. */
    val lensModel: String,
    /** Focal length in millimetres; `null` for a screenshot. */
    val focalLengthMm: Int?,
    /** ISO sensitivity; `null` for a screenshot. */
    val isoSpeed: Int?,
    /** Exposure time, spelled the way EXIF spells it (`1/250`); `null` for a screenshot. */
    val shutterSpeed: String?,
    /** Aperture as an f-number; `null` for a screenshot. */
    val aperture: Double?,
    /** The capture-time offset from UTC, as `+HH:MM`; `null` when none was recorded. */
    val offsetFromUtc: String?,
    /** The IANA zone the shot was taken in, when one is known. */
    val timeZoneId: String?,
    /** The lighting situation the exposure was chosen for. */
    val scene: CaptureScene,
)

/** Draws a coherent [MockExif] for one frame. */
object ExifSynthesis {
    /**
     * Builds the EXIF for a frame shot on [camera] in [scene].
     *
     * The draws are ordered so that the lens is chosen first and the aperture is then
     * clamped to what that lens can open to. Doing it the other way round — aperture first,
     * lens second — is what produces f/1.4 on a lens whose maximum is f/2.8.
     */
    fun synthesize(
        camera: CameraProfile,
        scene: CaptureScene,
        zoneId: String?,
        offsetSeconds: Int?,
        prng: Prng,
    ): MockExif {
        if (scene == CaptureScene.ScreenCapture) {
            return MockExif(
                make = camera.make,
                model = camera.model,
                lensModel = "",
                focalLengthMm = null,
                isoSpeed = null,
                shutterSpeed = null,
                aperture = null,
                offsetFromUtc = offsetSeconds?.let(::formatOffset),
                timeZoneId = zoneId,
                scene = scene,
            )
        }
        val lens = prng.pick(camera.lenses)
        val aperture = drawAperture(lens, scene, prng)
        return MockExif(
            make = camera.make,
            model = camera.model,
            lensModel = lens.model,
            focalLengthMm = lens.focalLengthsMm.takeIf { it.isNotEmpty() }?.let { prng.pick(it) },
            isoSpeed = roundToIsoStop(prng.nextInt(scene.isoRange.first, scene.isoRange.last)),
            shutterSpeed = "1/" + roundToShutterStop(drawShutter(scene, prng)),
            aperture = aperture,
            offsetFromUtc = offsetSeconds?.let(::formatOffset),
            timeZoneId = zoneId,
            scene = scene,
        )
    }

    /** A raw shutter denominator from the scene's band, before it is snapped to a real stop. */
    private fun drawShutter(
        scene: CaptureScene,
        prng: Prng,
    ): Int = prng.nextInt(scene.shutterDenominatorRange.first, scene.shutterDenominatorRange.last)

    /** The aperture the scene calls for, floored at what the mounted lens can open to. */
    private fun drawAperture(
        lens: LensProfile,
        scene: CaptureScene,
        prng: Prng,
    ): Double {
        val wanted = prng.nextDouble(scene.apertureRange.start, scene.apertureRange.endInclusive)
        val floor = if (lens.maxAperture > 0.0) lens.maxAperture else scene.apertureRange.start
        return nearestStop(maxOf(wanted, floor), APERTURE_STOPS)
    }

    /** Snaps to a real ISO stop; cameras do not record ISO 337. */
    private fun roundToIsoStop(value: Int): Int = ISO_STOPS.minByOrNull { kotlin.math.abs(it - value) } ?: value

    /** Snaps to a real shutter denominator; cameras do not record 1/337s. */
    private fun roundToShutterStop(value: Int): Int = SHUTTER_STOPS.minByOrNull { kotlin.math.abs(it - value) } ?: value

    private fun nearestStop(
        value: Double,
        stops: List<Double>,
    ): Double = stops.minByOrNull { kotlin.math.abs(it - value) } ?: value

    /** EXIF `OffsetTimeOriginal` spelling — `+09:00`, never a bare second count. */
    private fun formatOffset(offsetSeconds: Int): String {
        val sign = if (offsetSeconds < 0) '-' else '+'
        val absolute = kotlin.math.abs(offsetSeconds)
        return String.format(
            Locale.ROOT,
            "%c%02d:%02d",
            sign,
            absolute / SECONDS_PER_HOUR,
            (absolute % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE,
        )
    }

    private const val SECONDS_PER_HOUR = 3600
    private const val SECONDS_PER_MINUTE = 60

    private val ISO_STOPS =
        listOf(
            64,
            100,
            125,
            160,
            200,
            250,
            320,
            400,
            500,
            640,
            800,
            1000,
            1250,
            1600,
            2000,
            2500,
            3200,
            4000,
            5000,
            6400,
            8000,
            10000,
            12800,
        )

    private val SHUTTER_STOPS =
        listOf(
            15,
            20,
            25,
            30,
            40,
            50,
            60,
            80,
            100,
            125,
            160,
            200,
            250,
            320,
            400,
            500,
            640,
            800,
            1000,
            1250,
            1600,
            2000,
            2500,
            3200,
            4000,
        )

    private val APERTURE_STOPS =
        listOf(
            1.2,
            1.4,
            1.7,
            1.8,
            2.0,
            2.2,
            2.8,
            3.2,
            3.5,
            4.0,
            4.5,
            5.0,
            5.6,
            6.3,
            7.1,
            8.0,
            9.0,
            10.0,
            11.0,
            13.0,
            14.0,
            16.0,
        )
}
