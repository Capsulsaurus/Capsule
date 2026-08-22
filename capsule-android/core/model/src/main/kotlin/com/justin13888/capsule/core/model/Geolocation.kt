package com.justin13888.capsule.core.model

/**
 * Where a coordinate came from — the sidecar's `gps.source`.
 *
 * The distinction is load-bearing rather than informational: an [Inferred] value is written
 * to the canonical `gps` field only on explicit user confirmation, the same promotion rule
 * that governs an AI tag becoming a user tag. Without the tag there would be no way to stop
 * an automated guess silently overwriting capture truth.
 */
enum class GpsSource(
    override val wire: String,
) : WireEnum {
    /** Written by the capturing device. */
    Exif("exif"),

    /** Set by the user. */
    Manual("manual"),

    /** Client-derived — a paired device's location, or an ML suggestion. Needs confirmation. */
    Inferred("inferred"),
    ;

    companion object : WireCodec<GpsSource>({ entries })
}

/**
 * The coordinate datum a stored fix is expressed in — the sidecar's `gps.datum`.
 *
 * Coordinates are stored **verbatim in the datum the source supplied** and never converted
 * at rest, because GCJ-02 → WGS-84 has no exact inverse and converting on input would
 * destroy the user's ground truth. Display-side conversion happens on the way out, marked
 * approximate wherever it surfaces.
 *
 * BD-09 is deliberately absent: Baidu's datum exists only at the input edge, where it is
 * folded to [Gcj02] by an error-bounded iterative inverse held to a sub-metre bound, and is
 * never a storable value.
 */
enum class GpsDatum(
    override val wire: String,
) : WireEnum {
    /**
     * The near-universal camera datum, and the **wire-absent default**.
     *
     * A `gps` value carrying no `datum` key decodes as this, which is what keeps every
     * pre-`datum` sidecar and known-answer vector byte-identical.
     */
    Wgs84("wgs84"),

    /** China's legally mandated obfuscated datum, and the target of the BD-09 fold. */
    Gcj02("gcj02"),
    ;

    /** Whether this is the wire-absent default, and so omitted on encode. */
    val isWireAbsentDefault: Boolean get() = this == Wgs84

    companion object : WireCodec<GpsDatum>({ entries })
}

/**
 * A stored GPS fix — the sidecar's `gps` value, an atomic unit under CRDT merge.
 *
 * [datum] travels with [latitude] and [longitude] in one write on purpose: a merge that
 * could pair one device's coordinates with another's datum would silently relocate the
 * photo by the few hundred metres that separate the two systems.
 */
data class GpsFix(
    /** Degrees north, in [datum]. */
    val latitude: Double,
    /** Degrees east, in [datum]. */
    val longitude: Double,
    /** How the fix was established. */
    val source: Wire<GpsSource>,
    /** The datum the coordinates are expressed in; [GpsDatum.Wgs84] when wire-absent. */
    val datum: Wire<GpsDatum> = GpsDatum.Wgs84.asWire(),
) {
    /**
     * The fix rounded to two decimal places (≈1 km), the default when an asset crosses a
     * trust boundary.
     *
     * Precise coordinates are fingerprinting surface — they reveal a home address — so a
     * share link, an external backup handoff, or a federated fetch gets this unless the
     * user opts in per-export to full precision. The rounding happens at the moment of
     * export; the sidecar inside the library keeps full precision either way.
     */
    fun redactedForExport(): GpsFix =
        copy(
            latitude = roundToExportPrecision(latitude),
            longitude = roundToExportPrecision(longitude),
        )

    private companion object {
        /** Two decimal places — the ≈1 km bucket the privacy-on-export table specifies. */
        const val EXPORT_SCALE = 100.0

        fun roundToExportPrecision(degrees: Double): Double = Math.round(degrees * EXPORT_SCALE) / EXPORT_SCALE
    }
}
