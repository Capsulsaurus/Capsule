package com.justin13888.capsule.feature.albums.ui.common

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.RgbColor
import com.justin13888.capsule.core.ui.gallery.GalleryCellChrome
import com.justin13888.capsule.core.ui.gallery.GalleryCellMarkers
import com.justin13888.capsule.core.ui.gallery.GalleryCellSync

/**
 * One asset drawn from what this device already holds without fetching anything.
 *
 * The placeholder that rides inside the encrypted metadata blob is the first rung of the
 * representation ladder and costs no request at all, so a grid is browsable the instant
 * metadata syncs. Drawing from it here — rather than reaching for an image loader — also
 * keeps this module off the network entirely, which is what the gallery read path requires
 * rather than merely prefers.
 *
 * @param contentDescription what a screen reader calls this cell. Supplied by the caller
 *   because only the caller knows which listing the asset is being shown in.
 */
@Composable
internal fun AssetThumbnail(
    asset: Asset,
    contentDescription: String,
    modifier: Modifier = Modifier,
    markers: GalleryCellMarkers = GalleryCellMarkers.None,
    onClick: (() -> Unit)? = null,
) {
    val base = asset.lqip?.dominantColor.toComposeColor(MaterialTheme.colorScheme.surfaceVariant)
    val clickable = if (onClick == null) Modifier else Modifier.clickable(onClick = onClick)
    Box(
        modifier =
            modifier
                .fillMaxSize()
                .clip(RoundedCornerShape(THUMBNAIL_CORNER))
                .background(
                    // Two stops from the same stored colour: enough to read as a photograph
                    // rather than a swatch, without pretending to be pixels nobody fetched.
                    Brush.linearGradient(listOf(base, base.copy(alpha = PLACEHOLDER_FADE))),
                ).capsuleDescription(contentDescription)
                .then(clickable),
    ) {
        GalleryCellChrome(markers = markers)
    }
}

/** The chrome an asset carries in a listing, derived from its own state. */
internal fun Asset.galleryMarkers(): GalleryCellMarkers =
    GalleryCellMarkers(
        stackCount = null,
        videoDuration = null,
        isFavourite = rating >= FAVOURITE_RATING,
        sync = GalleryCellSync.None,
        isDegraded = !heldRepresentations.holds(RepresentationTier.Preview),
    )

private fun RgbColor?.toComposeColor(fallback: Color): Color =
    this?.let { Color(red = it.red, green = it.green, blue = it.blue) } ?: fallback

/** Rating at which the heart is drawn — the library's own idea of a favourite. */
private const val FAVOURITE_RATING = 5

/** How far the placeholder gradient fades, so a cell reads as one object rather than a fill. */
private const val PLACEHOLDER_FADE = 0.72f

private val THUMBNAIL_CORNER = 6.dp
