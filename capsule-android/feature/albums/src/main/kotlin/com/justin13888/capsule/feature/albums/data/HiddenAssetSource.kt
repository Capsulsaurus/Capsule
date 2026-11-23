package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.Asset

/**
 * Where the Hidden view reads its contents from.
 *
 * A port because `core:data` has no window onto hidden assets today: every listing it offers
 * — the timeline, album contents, search — excludes them by contract, which is exactly right
 * for those surfaces and leaves this one with nothing to call. The missing primitive is a
 * `hidden(offset, limit)` alongside `trash(offset, limit)` on the timeline contract, since
 * the two are the same shape and sit behind the same gate.
 *
 * Declaring the seam here rather than reaching around it keeps the screen honest: it is
 * written against the paged shape a real source will have, so the day that method lands the
 * change is one binding.
 */
interface HiddenAssetSource {
    /** A window of the assets the user hid, newest first. */
    suspend fun hidden(
        offset: Int,
        limit: Int,
    ): List<Asset>
}

/**
 * The stand-in until the timeline contract grows a hidden window.
 *
 * Returns nothing rather than guessing. A source that fell back to "assets the timeline
 * excluded for some reason" would put trashed and stack-suppressed files into a view the user
 * unlocked expecting to find what *they* hid.
 */
object EmptyHiddenAssetSource : HiddenAssetSource {
    override suspend fun hidden(
        offset: Int,
        limit: Int,
    ): List<Asset> = emptyList()
}
