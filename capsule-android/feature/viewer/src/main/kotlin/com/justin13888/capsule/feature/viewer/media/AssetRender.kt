package com.justin13888.capsule.feature.viewer.media

import androidx.compose.runtime.Immutable
import androidx.compose.ui.graphics.Color

/**
 * Everything the surface needs to draw one asset, with no repository in sight.
 *
 * The pixels are synthesized from the placeholder rather than decoded. That is not a
 * shortcut around a missing image loader so much as an honest rendering of what this client
 * actually holds for most assets most of the time: the placeholder rides inside the encrypted
 * metadata blob and arrives at zero extra request, which is exactly why the ladder always has
 * a floor and a missing derivative is a display problem rather than a blank frame.
 */
@Immutable
data class AssetRender(
    /** The placeholder's dominant colour, or `null` when the record carries no placeholder. */
    val dominantColor: Color?,
    /** Width over height. Squared when the extractor could not probe the file. */
    val aspectRatio: Float,
    /** Whether no decoder on this device claims the format. */
    val isUnreadable: Boolean = false,
)
