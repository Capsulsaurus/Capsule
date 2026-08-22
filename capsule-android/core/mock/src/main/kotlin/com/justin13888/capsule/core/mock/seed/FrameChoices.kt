package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.CullFlag
import java.time.LocalDateTime

/*
 * The weighted draws a frame's composition comes from.
 *
 * Split out of the emission pass because they answer a different question. Emission decides
 * how many assets a moment produces and how they are stacked; these decide what a single
 * frame looks like, and keeping the two apart means a change to the aspect mix cannot
 * accidentally alter how bursts are grouped.
 */

/**
 * Which shape this moment takes.
 *
 * Split in two so neither half carries the other's branches: the screenshot case is a
 * property of the day, the stacked shapes depend on how many frames are left, and the loose
 * shapes are what is left over when nothing grouped.
 */
internal fun LibraryGenerator.chooseShape(
    day: PlannedDay,
    camera: CameraProfile,
    remaining: Int,
): FrameShape {
    if (day.kind == DayKind.ScreenshotRun) return FrameShape.Screenshot
    return stackedShape(day, camera, remaining) ?: looseShape()
}

/**
 * The stack this moment forms, or `null` when it forms none.
 *
 * The branches run in order of decreasing specificity: a body that shoots RAW gets first
 * refusal on the pair, then the shapes that need several frames. Testing the cheap case first
 * would make the expensive shapes unreachable.
 */
private fun LibraryGenerator.stackedShape(
    day: PlannedDay,
    camera: CameraProfile,
    remaining: Int,
): FrameShape? {
    val canPair = remaining >= RAW_PAIR_SIZE && camera.shootsRaw
    val canBurst = remaining >= BURST_MIN && day.kind.weight >= BURST_MIN_DAY_WEIGHT
    val canLive = remaining >= LIVE_PHOTO_SIZE && camera === CameraBag.IPHONE_15_PRO
    return when {
        canPair && stackPrng.chance(RAW_PAIR_CHANCE) -> FrameShape.RawJpegPair
        canBurst && stackPrng.chance(BURST_CHANCE) -> FrameShape.Burst
        canLive && stackPrng.chance(LIVE_PHOTO_CHANCE) -> FrameShape.LivePhoto
        remaining >= CHAPTER_MIN && stackPrng.chance(CHAPTERED_CHANCE) -> FrameShape.ChapteredVideo
        else -> null
    }
}

/** A free-standing clip, or an ordinary still. */
private fun LibraryGenerator.looseShape(): FrameShape =
    if (stackPrng.chance(SINGLE_VIDEO_CHANCE)) FrameShape.SingleVideo else FrameShape.SingleFrame

/**
 * The lighting situation, weighted by the hour it was shot at.
 *
 * Time of day is the strongest predictor of a scene there is: a frame taken at two in the
 * morning is not a daylight landscape, and letting the hour steer the draw is what stops the
 * generated EXIF contradicting the generated timestamp.
 */
internal fun LibraryGenerator.drawScene(
    day: PlannedDay,
    at: LocalDateTime,
): CaptureScene {
    if (day.kind == DayKind.ScreenshotRun) return CaptureScene.ScreenCapture
    val hour = at.hour
    val candidates = CaptureScene.entries.filter { it != CaptureScene.ScreenCapture }
    val weights =
        candidates.map { scene ->
            when {
                hour in NIGHT_HOURS && scene == CaptureScene.NightCity -> scene.weight * NIGHT_BOOST
                hour in NIGHT_HOURS && scene == CaptureScene.Daylight -> scene.weight * DAYLIGHT_NIGHT_PENALTY
                hour in GOLDEN_HOURS && scene == CaptureScene.GoldenHour -> scene.weight * GOLDEN_BOOST
                else -> scene.weight
            }
        }
    return framePrng.pickWeighted(candidates, weights)
}

/**
 * A width-over-height ratio from the mix a real library actually has.
 *
 * Overwhelmingly 3:2 and 4:3 in both orientations, because that is what cameras and phones
 * produce; squares and panoramas exist but are rare. A uniform draw over the codec's whole
 * range would make every second tile a panorama and would tell a layout reviewer nothing
 * about how the grid behaves on real content.
 */
internal fun LibraryGenerator.drawAspect(camera: CameraProfile): Double =
    if (camera === CameraBag.FILM_SCANNER) {
        SCANNED_ASPECT
    } else {
        framePrng.pickWeighted(STILL_ASPECTS, STILL_ASPECT_WEIGHTS)
    }

/** The culling decision, which only a reviewed shoot actually carries. */
internal fun LibraryGenerator.drawCull(day: PlannedDay): CullFlag =
    if (day.kind != DayKind.StudioShoot) {
        CullFlag.Neutral
    } else {
        framePrng.pickWeighted(
            listOf(CullFlag.Pick, CullFlag.Neutral, CullFlag.Reject),
            listOf(PICK_WEIGHT, NEUTRAL_WEIGHT, REJECT_WEIGHT),
        )
    }

/** Stars. Most frames have none; a shoot's keepers have several. */
internal fun LibraryGenerator.drawRating(day: PlannedDay): Int =
    when {
        day.kind == DayKind.StudioShoot && framePrng.chance(STUDIO_RATED_SHARE) -> framePrng.nextInt(3, 5)
        framePrng.chance(CASUAL_RATED_SHARE) -> framePrng.nextInt(1, 5)
        else -> 0
    }

/** The still aspect mix: mostly 3:2 and 4:3 either way up, with square and panorama tails. */
private val STILL_ASPECTS =
    listOf(3.0 / 2, 2.0 / 3, 4.0 / 3, 3.0 / 4, 16.0 / 9, 1.0, 3.0 / 1, 2.4 / 1)

private val STILL_ASPECT_WEIGHTS = listOf(27.0, 18.0, 17.0, 14.0, 7.0, 6.0, 1.6, 1.2)

private const val NIGHT_BOOST = 6.0
private const val DAYLIGHT_NIGHT_PENALTY = 0.05
private const val GOLDEN_BOOST = 5.0
private const val SCANNED_ASPECT = 1.5
private const val PICK_WEIGHT = 0.22
private const val NEUTRAL_WEIGHT = 0.45
private const val REJECT_WEIGHT = 0.33
private const val STUDIO_RATED_SHARE = 0.35
private const val CASUAL_RATED_SHARE = 0.04
private val NIGHT_HOURS = 0..6
private val GOLDEN_HOURS = 18..20
private const val RAW_PAIR_CHANCE = 0.22
private const val BURST_CHANCE = 0.10
private const val BURST_MIN_DAY_WEIGHT = 3.0
private const val LIVE_PHOTO_CHANCE = 0.12
private const val CHAPTERED_CHANCE = 0.015
private const val SINGLE_VIDEO_CHANCE = 0.055
