package com.justin13888.capsule.core.model

/**
 * The kind of media an asset holds — the catalog's `AssetRecord.asset_type`.
 *
 * This is the coarse discriminator the timeline filters on (`Catalog.query_timeline_filtered`
 * takes it as a raw token), not the precise format: that is [ContentType].
 */
enum class AssetType(
    override val wire: String,
) : WireEnum {
    /** A still image, whatever its container. */
    Photo("photo"),

    /** A moving-image file, with or without an audio track. */
    Video("video"),
    ;

    companion object : WireCodec<AssetType>({ entries })
}

/**
 * The `media_kind` query field — `image | video`, derived from [ContentType].
 *
 * Deliberately a separate set from [AssetType] even though the two agree in every case v1
 * ships: `media_kind` is a *derived* smart-album query field whose vocabulary is fixed by
 * the predicate grammar, while `asset_type` is a stored catalog column. Folding them into
 * one enum would tie a stored column's value set to a query grammar's, so a later grammar
 * revision could not add a kind without a schema migration it has no business forcing.
 */
enum class MediaKind(
    override val wire: String,
) : WireEnum {
    /** Derived from any image content type. */
    Image("image"),

    /** Derived from any video content type. */
    Video("video"),
    ;

    companion object : WireCodec<MediaKind>({ entries })
}

/**
 * The sidecar's `content_type` — MIME syntax, exactly **one canonical value per format**.
 *
 * Aliases are not members of the set: `image/jpg` is a rejection, not a synonym for
 * `image/jpeg`. One spelling per format is what keeps two clients' canonical CBOR
 * byte-identical, and byte-identical CBOR is what keeps their signatures verifiable across
 * peers — an alias tolerated on one platform would make an honest sidecar look forged on
 * another.
 *
 * Adding a format requires a new, later-dated `protocol_version`; old albums never see it.
 */
enum class ContentType(
    override val wire: String,
    val mediaKind: MediaKind,
) : WireEnum {
    /** JPEG — the near-universal camera output. */
    Jpeg("image/jpeg", MediaKind.Image),

    /** PNG. */
    Png("image/png", MediaKind.Image),

    /** WebP. */
    Webp("image/webp", MediaKind.Image),

    /** GIF. */
    Gif("image/gif", MediaKind.Image),

    /** TIFF. */
    Tiff("image/tiff", MediaKind.Image),

    /** HEIC — the default iPhone capture format. */
    Heic("image/heic", MediaKind.Image),

    /** AVIF. */
    Avif("image/avif", MediaKind.Image),

    /** JPEG XL. */
    Jxl("image/jxl", MediaKind.Image),

    /** Adobe DNG — the RAW half of a RAW+JPEG pair. */
    Dng("image/x-adobe-dng", MediaKind.Image),

    /** MP4. */
    Mp4("video/mp4", MediaKind.Video),

    /** QuickTime — the `.mov` an iPhone Live Photo's motion half arrives as. */
    QuickTime("video/quicktime", MediaKind.Video),

    /** Matroska. */
    Matroska("video/x-matroska", MediaKind.Video),

    /** WebM. */
    Webm("video/webm", MediaKind.Video),
    ;

    companion object : WireCodec<ContentType>({ entries })
}

/**
 * How an asset's capture-time offset from UTC was established — `capture_tz_source`.
 *
 * Worth keeping because the three are not equally trustworthy, and a timeline that sorted
 * them as if they were would put a photo on the wrong day. [OffsetExif] came from the
 * camera; [GpsLookup] was inferred from where the shot was taken; [Floating] means no
 * offset is known at all and the wall-clock reading is all there is.
 */
enum class CaptureTzSource(
    override val wire: String,
) : WireEnum {
    /** The capture file carried an explicit UTC offset. */
    OffsetExif("offset_exif"),

    /** Derived by looking the capture coordinates up in the timezone database. */
    GpsLookup("gps_lookup"),

    /** No offset is known; the timestamp is a floating local wall-clock reading. */
    Floating("floating"),
    ;

    companion object : WireCodec<CaptureTzSource>({ entries })
}

/**
 * The culling flag — the review pass a photographer makes after a shoot.
 *
 * Deliberately orthogonal to the numeric star `rating`: a reject can carry three stars, and
 * a tool that conflates the two forces a lossy workflow. [Neutral] is the never-flagged
 * default and is wire-absent, so an asset that has never been reviewed is indistinguishable
 * from one explicitly marked neutral — which is the intent, since neither is a decision.
 */
enum class CullFlag(
    override val wire: String,
) : WireEnum {
    /** Keep — promote into an album or a share. */
    Pick("pick"),

    /** Undecided. The never-flagged default; wire-absent. */
    Neutral("neutral"),

    /** Toss — the batch that a reject-sweep moves to trash. */
    Reject("reject"),
    ;

    companion object : WireCodec<CullFlag>({ entries })
}

/**
 * The derived cull state of a group — a stack or a burst.
 *
 * A group stores no flag of its own; its state is computed from its members every time it
 * is displayed. That is the point: a stored group flag would be a second source of truth
 * able to disagree with the members it summarises, and there is no correct way to resolve
 * that disagreement.
 */
enum class GroupCullState {
    /** Every member is [CullFlag.Reject] — the whole group can be swept. */
    AllRejected,

    /** At least one member is [CullFlag.Pick]. */
    AnyPick,

    /** Neither — members disagree, or none has been reviewed. */
    Mixed,
}

/**
 * Whether an import copies its source file or takes it over — `ImportMode`.
 *
 * [Move] is the mode with teeth: it releases the user's only copy, so it is gated behind
 * verify-before-destroy and is mutually exclusive with a staged upload policy that defers
 * exactly the upload that release depends on.
 */
enum class ImportMode(
    override val wire: String,
) : WireEnum {
    /** Leave the source file in place. */
    Copy("copy"),

    /** Take the source file over, releasing it once durable storage is confirmed. */
    Move("move"),
    ;

    companion object : WireCodec<ImportMode>({ entries })
}
