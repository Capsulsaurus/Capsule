package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.MediaKind
import com.justin13888.capsule.core.model.StackType
import com.justin13888.capsule.core.model.knownOrNull

/**
 * The built-in view albums the Media Types section offers.
 *
 * Each is a predicate over what the local index already holds, evaluated at render time and
 * stored nowhere — the design's definition of a view album. Keeping them as a closed enum
 * rather than as seeded smart-album definitions matters: a system view must exist on a
 * brand-new account that has never opened the smart-album builder, and it must not be
 * deletable by an accident in the smart-album list.
 *
 * @param token the stable spelling that travels inside a [CollectionRef].
 */
enum class SystemViewKind(
    val token: String,
) {
    /** Anything the extractor called a video. */
    Videos("videos"),

    /** A still and its short motion clip, grouped as one interactive unit. */
    LivePhotos("live_photos"),

    /** High-speed still sequences. */
    Bursts("bursts"),

    /** Sequences meant to be stitched into one wide-field image. */
    Panoramas("panoramas"),

    /** Captures whose RAW half is stored alongside a processed file. */
    Raw("raw"),
    ;

    /**
     * Whether [asset] belongs in this view.
     *
     * Read off the asset's own projections rather than off a query, because a view has to be
     * computable from whatever the local index holds while offline — a gallery read path
     * performs no network I/O at all.
     */
    fun matches(asset: Asset): Boolean =
        when (this) {
            Videos -> asset.mediaKindOrNull() == MediaKind.Video
            LivePhotos -> asset.stackTypeOrNull() == StackType.LivePhoto
            Bursts -> asset.stackTypeOrNull() == StackType.Burst
            Panoramas -> asset.stackTypeOrNull() == StackType.Panorama
            Raw -> asset.stackTypeOrNull() == StackType.RawJpeg
        }

    companion object {
        /** The kind [token] names, or `null` when it is outside this build's set. */
        fun fromToken(token: String): SystemViewKind? = entries.firstOrNull { it.token == token }
    }
}

/** The media kind an asset's content type resolves to, or `null` when it is unknown. */
private fun Asset.mediaKindOrNull(): MediaKind? = contentType?.knownOrNull?.mediaKind

/** The stack type this asset participates in, or `null` when it is not stacked. */
private fun Asset.stackTypeOrNull(): StackType? = stack?.stackType?.knownOrNull
