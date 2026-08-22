package com.justin13888.capsule.core.model

/**
 * A rung of the representation ladder, cheapest first.
 *
 * The client fetches the smallest representation that satisfies the user's current intent
 * and nothing more — that frugality is the whole design of the download path, and it only
 * works if "which rung" is an ordered value rather than a set of independent booleans.
 */
enum class RepresentationTier(
    override val wire: String,
    val rank: Int,
) : WireEnum {
    /**
     * The placeholder embedded in the encrypted metadata blob.
     *
     * Costs no request at all — it arrives with metadata — which is why it is the ladder's
     * floor and why an asset is never left with nothing to draw.
     */
    Lqip("lqip", 0),

    /** A grid-sized derivative (~256 px long edge), fetched as the asset nears the viewport. */
    Thumbnail("thumbnail", 1),

    /** A screen-resolution derivative, fetched when the asset is opened. */
    Preview("preview", 2),

    /** The original bytes, fetched only on explicit demand. */
    Original("original", 3),
    ;

    companion object : WireCodec<RepresentationTier>({ entries })
}

/**
 * Which rungs this device currently holds for an asset.
 *
 * [Lqip][RepresentationTier.Lqip] is always held once metadata has synced, so the ladder
 * always has a floor to degrade to and a missing derivative never leaves a blank tile.
 */
@JvmInline
value class HeldRepresentations(
    val tiers: Set<RepresentationTier>,
) {
    /** The best rung available locally right now. */
    val best: RepresentationTier
        get() = tiers.maxByOrNull(RepresentationTier::rank) ?: RepresentationTier.Lqip

    /** Whether [tier] can be served without a network request. */
    fun holds(tier: RepresentationTier): Boolean = tier in tiers

    /**
     * The best rung at or below [wanted] that is held locally — what to draw when a fetch
     * above the current tier fails permanently.
     *
     * Degradation walks *down* (preview → thumbnail → LQIP) and stops at the floor. It never
     * removes the asset's metadata or index entry: a missing derivative is a display problem,
     * not a reason to make the user's photo disappear from their library.
     */
    fun degradeFrom(wanted: RepresentationTier): RepresentationTier =
        tiers.filter { it.rank <= wanted.rank }.maxByOrNull(RepresentationTier::rank)
            ?: RepresentationTier.Lqip

    companion object {
        /** Metadata has synced and nothing else has been fetched. */
        val LQIP_ONLY = HeldRepresentations(setOf(RepresentationTier.Lqip))
    }
}

/**
 * How much of each new asset this library fetches by default.
 *
 * Anything above the configured tier is fetched lazily, on demand. The original is never
 * fetched speculatively unless this device was its uploader, in which case it already holds
 * the plaintext and downloads nothing.
 */
enum class SyncScope(
    override val wire: String,
    val tier: RepresentationTier,
) : WireEnum {
    /** Metadata only — including the LQIP, which is enough to render a browsable grid. */
    MetadataOnly("metadata_only", RepresentationTier.Lqip),

    /** Metadata plus grid thumbnails. */
    MetadataAndThumbnails("metadata_thumbnails", RepresentationTier.Thumbnail),

    /** Metadata, thumbnails, and the original bytes. */
    MetadataThumbnailsAndOriginal("metadata_thumbnails_original", RepresentationTier.Original),
    ;

    companion object : WireCodec<SyncScope>({ entries })
}

/**
 * The encoding a derivative blob is stored in.
 *
 * A format outside this set is a structural rejection, not an unknown to skip — a derivative
 * whose format cannot be named cannot be decoded, and guessing would be worse than refusing.
 * There is deliberately no JPEG fallback.
 */
enum class DerivativeFormat(
    override val wire: String,
) : WireEnum {
    /** The committed primary master format for stills. */
    Jxl("jxl"),

    /** The universal delivery format. */
    Avif("avif"),

    /** Last-resort delivery fallback. */
    Webp("webp"),

    /** Video preview transcode — H.264 baseline, capped to 1080p. */
    H264Baseline("h264_baseline"),

    /**
     * The sentinel meaning "the original was already smaller than this tier would be".
     *
     * Distinct from a *missing* derivative, which is treated as rebuildable: this one says
     * the derivative deliberately does not exist and no regeneration is owed.
     */
    OriginalSentinel("original"),
    ;

    companion object : WireCodec<DerivativeFormat>({ entries })
}

/**
 * Why an above-tier fetch did not succeed.
 *
 * Three classes, not two, and the third is the reason this is an enum rather than a boolean:
 * a `403` is neither a durability loss nor a network blip. It signals that the user's access
 * to the album changed, so the client re-syncs membership before retrying — surfacing a
 * revocation as a revocation rather than masking it as a missing file.
 */
enum class FetchFailureClass(
    override val wire: String,
) : WireEnum {
    /** A network drop or `5xx`. Retries with backoff, resuming by byte range. */
    Transient("transient"),

    /** `410 Gone`, a purged origin, an unreachable home server. Degrades to the best held rung. */
    Permanent("permanent"),

    /** A `403`. Re-sync membership and capability first, and only then degrade. */
    AuthorizationChanged("authorization_changed"),
    ;

    companion object : WireCodec<FetchFailureClass>({ entries })
}
