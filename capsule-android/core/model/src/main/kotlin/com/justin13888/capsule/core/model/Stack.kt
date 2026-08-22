package com.justin13888.capsule.core.model

import java.time.Instant

/**
 * The kind of grouping a stack represents — a closed enum per `protocol_version`.
 *
 * Adding a type requires a new, later-dated version, and old albums never see it. v1
 * auto-detection covers [RawJpeg] pairing only; the rest are manual-only until their
 * detectors ship, which is a scheduling fact and not a schema one — every type below is
 * normative now, so a sidecar written today survives the detectors landing.
 */
enum class StackType(
    override val wire: String,
) : WireEnum {
    /** The uncompressed RAW and its processed JPEG, treated as one asset. */
    RawJpeg("raw_jpeg"),

    /** A high-speed sequence of stills with one "best photo" in front. */
    Burst("burst"),

    /** A still paired with its 1.5–3 second motion clip. */
    LivePhoto("live_photo"),

    /** An image paired with its depth map, so bokeh stays adjustable after the shot. */
    Portrait("portrait"),

    /** AI grouping of visually similar images taken seconds apart. */
    SmartSelection("smart_selection"),

    /** Multiple exposures of one scene to be merged into an HDR image. */
    HdrBracket("hdr_bracket"),

    /** Shots at shifting focus points, merged for macro depth of field. */
    FocusStack("focus_stack"),

    /** Sensor-shifted captures merged for resolution and colour. */
    PixelShift("pixel_shift"),

    /** A sequence intended to be stitched into one wide-field image. */
    Panorama("panorama"),

    /** A heavy master paired with a lightweight editing proxy. */
    Proxy("proxy"),

    /** Chunks of one long recording an action camera split at a file-size limit. */
    Chaptered("chaptered"),

    /** Video grouped with externally recorded audio by timecode or waveform. */
    DualAudio("dual_audio"),

    /** A grouping the user made that fits none of the detected shapes. */
    Custom("custom"),
    ;

    companion object : WireCodec<StackType>({ entries })
}

/**
 * An asset's role inside its stack — the sidecar's `stack_membership.role`.
 *
 * [Primary] is a pointer, not a promotion: choosing the best photo of a burst writes this
 * field and nothing else, so the operation is fully reversible and cannot lose bytes.
 */
enum class StackRole(
    override val wire: String,
) : WireEnum {
    /** The one member a collapsed stack shows. */
    Primary("primary"),

    /** An ordinary member, suppressed while the stack is collapsed. */
    Member("member"),

    /** A lightweight stand-in for a heavier master. */
    Proxy("proxy"),
    ;

    companion object : WireCodec<StackRole>({ entries })
}

/**
 * The catalog's `StackMemberRecord.member_role` — what *kind* of file a member is.
 *
 * Deliberately a wider set than [StackRole], which answers a different question. [StackRole]
 * says which member the collapsed stack shows; this says what the file is, and a stack needs
 * both: the RAW half of a pair is [Raw] whether or not it is also the primary.
 */
enum class StackMemberRole(
    override val wire: String,
) : WireEnum {
    /** The member the collapsed stack displays. */
    Primary("primary"),

    /** The uncompressed sensor data. */
    Raw("raw"),

    /** The moving-image half — a Live Photo's clip, or a chaptered segment. */
    Video("video"),

    /** Externally recorded audio matched by timecode or waveform. */
    Audio("audio"),

    /** A depth map accompanying a portrait capture. */
    DepthMap("depth_map"),

    /** A rendered or developed output. */
    Processed("processed"),

    /** The input a processed member was derived from. */
    Source("source"),

    /** Another rendition of the same capture. */
    Alternate("alternate"),

    /** A companion metadata file such as an XMP sidecar. */
    Sidecar("sidecar"),

    /** A lightweight editing stand-in. */
    Proxy("proxy"),

    /** The full-fidelity file a proxy stands in for. */
    Master("master"),
    ;

    companion object : WireCodec<StackMemberRole>({ entries })
}

/**
 * How a stack was detected — auto-grouping is best-effort and the user deserves to know
 * which guess produced a grouping they did not make.
 */
enum class DetectionMethod(
    override val wire: String,
) : WireEnum {
    /** Two files sharing a filename stem, e.g. `DSCF0001.RAF` and `DSCF0001.JPG`. */
    FilenameStem("filename_stem"),

    /** A content identifier written by the capturing device, as Live Photos carry. */
    ContentIdentifier("content_identifier"),

    /** Matching embedded timecode, as dual-system audio uses. */
    Timecode("timecode"),

    /** The user grouped these by hand. */
    Manual("manual"),
    ;

    companion object : WireCodec<DetectionMethod>({ entries })
}

/**
 * One asset's membership in a stack — the projection of the sidecar's `stack_membership`
 * LWW register onto the catalog's `stack_id` / `is_stack_hidden` columns.
 *
 * The register's value domain includes "no membership" (a stamped absence), which is why
 * joining, moving between, and leaving stacks are all the same kind of write and converge
 * identically no matter what order they arrive in.
 */
data class StackMembership(
    /** `AssetRecord.stack_id`. */
    val stackId: StackId,
    /** What kind of grouping this is. */
    val stackType: Wire<StackType>,
    /** Whether this member is the one a collapsed stack shows. */
    val role: Wire<StackRole>,
    /** Ordering within the stack — burst sequence, chapter index. */
    val memberIndex: Int? = null,
    /**
     * `AssetRecord.is_stack_hidden` — this member is suppressed from default views because
     * the stack is collapsed and it is not the primary.
     *
     * Distinct from [Asset.isHidden]: that is a user decision, this is a consequence of
     * stacking, and conflating them would make un-stacking silently un-hide something the
     * user had deliberately hidden.
     */
    val isSuppressed: Boolean = false,
)

/** One row of a stack's membership list — the catalog's `StackMemberRecord`. */
data class StackMember(
    /** `StackMemberRecord.id`. */
    val id: StackMemberId,
    /** `StackMemberRecord.stack_id`. */
    val stackId: StackId,
    /** `StackMemberRecord.asset_id`. */
    val assetId: AssetId,
    /** `StackMemberRecord.sequence_order`. */
    val sequenceOrder: Int,
    /** `StackMemberRecord.member_role`. */
    val memberRole: Wire<StackMemberRole>,
    /** `StackMemberRecord.created_at`. */
    val createdAt: Instant,
)

/**
 * A stack — the catalog's `AssetStackRecord`, field for field, plus how it was detected.
 *
 * Everything here is metadata. A stack edit rewrites the members' sidecars and emits a
 * provenance record per affected asset; it never deletes, rewrites, or merges the underlying
 * bytes, so a buggy or hostile stack edit cannot lose an original.
 */
data class Stack(
    /** `AssetStackRecord.id`. */
    val id: StackId,
    /** `AssetStackRecord.stack_type`. */
    val stackType: Wire<StackType>,
    /** `AssetStackRecord.primary_asset_id` — the member a collapsed stack shows. */
    val primaryAssetId: AssetId,
    /** `AssetStackRecord.cover_asset_id`; falls back to [primaryAssetId] when absent. */
    val coverAssetId: AssetId? = null,
    /** `AssetStackRecord.is_collapsed`. */
    val isCollapsed: Boolean = true,
    /** `AssetStackRecord.is_auto_generated` — false once the user has touched it. */
    val isAutoGenerated: Boolean = false,
    /** `AssetStackRecord.created_at`. */
    val createdAt: Instant,
    /** `AssetStackRecord.modified_at`. */
    val modifiedAt: Instant,
    /** Which detector produced the grouping, when it was not the user. */
    val detectedBy: Wire<DetectionMethod>? = null,
) {
    /** The asset to show in a grid — the explicit cover, else the primary. */
    val displayAssetId: AssetId get() = coverAssetId ?: primaryAssetId
}

/**
 * Derives a group's cull state from its members.
 *
 * A stack stores no flag of its own, so this is the only way to ask the question — and
 * deriving it means there is no second source of truth able to disagree with the members it
 * summarises. An empty group is [GroupCullState.Mixed]: nothing has been decided about it.
 */
fun deriveGroupCullState(memberFlags: Iterable<Wire<CullFlag>>): GroupCullState {
    val known = memberFlags.map { it.knownOrNull }
    if (known.isEmpty()) return GroupCullState.Mixed
    return when {
        known.all { it == CullFlag.Reject } -> GroupCullState.AllRejected
        known.any { it == CullFlag.Pick } -> GroupCullState.AnyPick
        else -> GroupCullState.Mixed
    }
}
