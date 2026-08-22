package com.justin13888.capsule.core.mock.seed

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ContentType
import com.justin13888.capsule.core.model.DetectionMethod
import com.justin13888.capsule.core.model.Stack
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.core.model.StackMember
import com.justin13888.capsule.core.model.StackMemberId
import com.justin13888.capsule.core.model.StackMemberRole
import com.justin13888.capsule.core.model.StackMembership
import com.justin13888.capsule.core.model.StackRole
import com.justin13888.capsule.core.model.StackType
import com.justin13888.capsule.core.model.asWire
import java.time.Duration
import java.time.ZoneOffset

/** How many assets a RAW+JPEG pair produces. */
internal const val RAW_PAIR_SIZE = 2

/** The shortest burst worth generating. */
internal const val BURST_MIN = 6

/** How many assets a Live Photo produces. */
internal const val LIVE_PHOTO_SIZE = 2

/** The fewest chapters a split recording produces. */
internal const val CHAPTER_MIN = 2

/**
 * The uncompressed RAW and its processed JPEG, as one asset.
 *
 * The JPEG is the primary and the DNG is suppressed, because that is the pair the user is
 * actually looking at: a collapsed stack showing the RAW would put an undeveloped frame in
 * the grid. Both keep their own bytes — a stack is metadata, and nothing here can lose the
 * negative.
 */
internal fun LibraryGenerator.emitRawJpegPair(context: GroupContext): Int {
    val stackId = StackId(DeterministicIds.uuidV7(context.at.toInstant(ZoneOffset.UTC).toEpochMilli(), idPrng))
    val aspect = 3.0 / 2
    val scene = CaptureScene.Daylight
    val jpeg =
        makeAsset(
            frameOf(context, scene, ContentType.Jpeg, aspect)
                .copy(membership = membershipOf(stackId, StackType.RawJpeg, StackRole.Primary, 0, false)),
        )
    val raw =
        makeAsset(
            frameOf(context, scene, ContentType.Dng, aspect)
                .copy(membership = membershipOf(stackId, StackType.RawJpeg, StackRole.Member, 1, true)),
        )
    addAsset(jpeg)
    addAsset(raw)
    // `filename_stem`: the two files came off the card as DSCF0001.JPG and DSCF0001.RAF, which
    // is the only auto-detection v1 actually ships.
    recordStack(stackId, StackType.RawJpeg, jpeg.id, listOf(jpeg, raw), DetectionMethod.FilenameStem)
    memberRoles(stackId, listOf(jpeg.id to StackMemberRole.Primary, raw.id to StackMemberRole.Raw))
    return RAW_PAIR_SIZE
}

/**
 * A high-speed sequence with one chosen best frame.
 *
 * The primary is drawn rather than fixed at the first frame: choosing the keeper is the whole
 * point of a burst, and a fixture where it is always index zero would let a bug that ignores
 * `primary_asset_id` pass every review.
 */
internal fun LibraryGenerator.emitBurst(
    context: GroupContext,
    remaining: Int,
): Int {
    val size = stackPrng.nextInt(BURST_MIN, BURST_MAX).coerceAtMost(remaining)
    val stackId = StackId(DeterministicIds.uuidV7(context.at.toInstant(ZoneOffset.UTC).toEpochMilli(), idPrng))
    val primaryIndex = stackPrng.nextInt(size)
    val scene = if (context.day.kind == DayKind.StudioShoot) CaptureScene.Action else CaptureScene.Daylight
    val frames =
        (0 until size).map { index ->
            val at = context.at.plusNanos(index.toLong() * BURST_INTERVAL_NANOS)
            makeAsset(
                frameOf(context.copy(at = at), scene, ContentType.Jpeg, 3.0 / 2).copy(
                    membership =
                        membershipOf(
                            stackId = stackId,
                            type = StackType.Burst,
                            role = if (index == primaryIndex) StackRole.Primary else StackRole.Member,
                            index = index,
                            suppressed = index != primaryIndex,
                        ),
                ),
            )
        }
    frames.forEach(::addAsset)
    recordStack(stackId, StackType.Burst, frames[primaryIndex].id, frames, DetectionMethod.Timecode)
    memberRoles(
        stackId,
        frames.mapIndexed { index, asset ->
            asset.id to if (index == primaryIndex) StackMemberRole.Primary else StackMemberRole.Alternate
        },
    )
    return size
}

/**
 * A still and the short clip captured around it.
 *
 * Detected by the content identifier the capturing device writes into both halves, which is
 * why the motion clip has no filename relationship to the still.
 */
internal fun LibraryGenerator.emitLivePhoto(context: GroupContext): Int {
    val stackId = StackId(DeterministicIds.uuidV7(context.at.toInstant(ZoneOffset.UTC).toEpochMilli(), idPrng))
    val scene = drawLiveScene(context)
    val still =
        makeAsset(
            frameOf(context, scene, ContentType.Heic, 4.0 / 3)
                .copy(membership = membershipOf(stackId, StackType.LivePhoto, StackRole.Primary, 0, false)),
        )
    val motion =
        makeAsset(
            frameOf(context, scene, ContentType.QuickTime, 4.0 / 3).copy(
                duration = Duration.ofMillis(stackPrng.nextInt(LIVE_MILLIS_MIN, LIVE_MILLIS_MAX).toLong()),
                membership = membershipOf(stackId, StackType.LivePhoto, StackRole.Member, 1, true),
            ),
        )
    addAsset(still)
    addAsset(motion)
    recordStack(stackId, StackType.LivePhoto, still.id, listOf(still, motion), DetectionMethod.ContentIdentifier)
    memberRoles(stackId, listOf(still.id to StackMemberRole.Primary, motion.id to StackMemberRole.Video))
    return LIVE_PHOTO_SIZE
}

/**
 * One long recording the camera split at a file-size limit.
 *
 * Chapters are consecutive rather than simultaneous, so their capture timestamps advance by
 * each chapter's own runtime — a chaptered stack whose members all share one instant would
 * hide exactly the ordering bug `sequence_order` exists to catch.
 */
internal fun LibraryGenerator.emitChapteredVideo(
    context: GroupContext,
    remaining: Int,
): Int {
    val chapters = stackPrng.nextInt(CHAPTER_MIN, CHAPTER_MAX).coerceAtMost(remaining)
    val stackId = StackId(DeterministicIds.uuidV7(context.at.toInstant(ZoneOffset.UTC).toEpochMilli(), idPrng))
    var at = context.at
    val frames =
        (0 until chapters).map { index ->
            val runtime = Duration.ofSeconds(stackPrng.nextInt(CHAPTER_SECONDS_MIN, CHAPTER_SECONDS_MAX).toLong())
            val frame =
                makeAsset(
                    frameOf(context.copy(at = at), CaptureScene.Daylight, ContentType.Mp4, 16.0 / 9).copy(
                        duration = runtime,
                        membership =
                            membershipOf(
                                stackId = stackId,
                                type = StackType.Chaptered,
                                role = if (index == 0) StackRole.Primary else StackRole.Member,
                                index = index,
                                suppressed = index != 0,
                            ),
                    ),
                )
            at = at.plusSeconds(runtime.seconds)
            frame
        }
    frames.forEach(::addAsset)
    recordStack(stackId, StackType.Chaptered, frames.first().id, frames, DetectionMethod.Timecode)
    memberRoles(stackId, frames.map { it.id to StackMemberRole.Video })
    return chapters
}

/** A frame spec for a group member, with the group's shared arguments already filled in. */
private fun frameOf(
    context: GroupContext,
    scene: CaptureScene,
    contentType: ContentType,
    aspectRatio: Double,
): FrameSpec =
    FrameSpec(
        day = context.day,
        localTime = context.at,
        camera = context.camera,
        scene = scene,
        contentType = contentType,
        aspectRatio = aspectRatio,
        albumId = context.album,
        routeFraction = context.fraction,
    )

private fun membershipOf(
    stackId: StackId,
    type: StackType,
    role: StackRole,
    index: Int,
    suppressed: Boolean,
): StackMembership =
    StackMembership(
        stackId = stackId,
        stackType = type.asWire(),
        role = role.asWire(),
        memberIndex = index,
        isSuppressed = suppressed,
    )

/** A Live Photo is shot on a phone, so it follows the hour rather than the day's kind. */
private fun LibraryGenerator.drawLiveScene(context: GroupContext): CaptureScene =
    if (context.at.hour in LIVE_INDOOR_HOURS) CaptureScene.IndoorAmbient else CaptureScene.Daylight

/** Records the stack itself. */
private fun LibraryGenerator.recordStack(
    stackId: StackId,
    type: StackType,
    primaryAssetId: AssetId,
    members: List<Asset>,
    detectedBy: DetectionMethod,
) {
    val createdAt = members.first().importTimestamp
    stacks +=
        Stack(
            id = stackId,
            stackType = type.asWire(),
            primaryAssetId = primaryAssetId,
            coverAssetId = primaryAssetId,
            isCollapsed = true,
            isAutoGenerated = detectedBy != DetectionMethod.Manual,
            createdAt = createdAt,
            modifiedAt = createdAt,
            detectedBy = detectedBy.asWire(),
        )
}

/** Records each member row, in sequence order. */
private fun LibraryGenerator.memberRoles(
    stackId: StackId,
    roles: List<Pair<AssetId, StackMemberRole>>,
) {
    val rows = stackMembers.getOrPut(stackId) { mutableListOf() }
    roles.forEachIndexed { index, (assetId, role) ->
        rows +=
            StackMember(
                id = StackMemberId(DeterministicIds.uuidV7(index.toLong(), idPrng)),
                stackId = stackId,
                assetId = assetId,
                sequenceOrder = index,
                memberRole = role.asWire(),
                createdAt = assetOf(assetId)?.importTimestamp ?: spec.now,
            )
    }
}

/** The longest burst generated. */
private const val BURST_MAX = 22

/** Frames a hundred milliseconds apart — a real camera's burst cadence. */
private const val BURST_INTERVAL_NANOS = 100_000_000L

private const val LIVE_MILLIS_MIN = 1_500
private const val LIVE_MILLIS_MAX = 3_000
private const val CHAPTER_MAX = 5
private const val CHAPTER_SECONDS_MIN = 240
private const val CHAPTER_SECONDS_MAX = 1_200
private val LIVE_INDOOR_HOURS = 17..23
