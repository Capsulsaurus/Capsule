package com.justin13888.capsule.feature.people

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.ui.gallery.GalleryCellChrome
import com.justin13888.capsule.core.ui.gallery.GalleryCellMarkers
import com.justin13888.capsule.core.ui.gallery.GalleryDefaults
import com.justin13888.capsule.core.ui.gallery.GalleryDensityLevels
import com.justin13888.capsule.core.ui.gallery.JustifiedGallery
import com.justin13888.capsule.core.ui.gallery.LayoutPlan
import com.justin13888.capsule.core.ui.gallery.LayoutPlanner
import com.justin13888.capsule.core.ui.gallery.rememberGalleryState
import kotlinx.collections.immutable.ImmutableList

/** Test handle for a photo grid, which has no text of its own to address it by. */
const val PEOPLE_GRID_TAG: String = "people.grid"

/** The alpha byte of an opaque `0xRRGGBB` fill. */
private const val OPAQUE_ALPHA = 0xFF000000.toInt()

/**
 * A grid of photographs.
 *
 * The library's own justified gallery, not a second grid: these are the same photographs at
 * the same aspect ratios the timeline draws, and a uniform grid here would crop every one of
 * them differently from the screen the user just came from.
 *
 * One section and no header. A person's photographs and a smart album's are both a single
 * run — the grouping the user asked for *is* the section — so the header height is solved to
 * zero rather than a date band being invented for it.
 *
 * @param contentDescription what a screen reader calls the grid as a whole.
 * @param cellDescription the announced label for one photograph, by index.
 */
@Composable
fun PeopleGrid(
    photos: ImmutableList<PersonPhoto>,
    contentDescription: String,
    cellDescription: (Int) -> String,
    onOpenAsset: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (photos.isEmpty()) return
    BoxWithConstraints(modifier = modifier.testTag(PEOPLE_GRID_TAG)) {
        val plan = rememberPhotoPlan(photos, maxWidth)
        JustifiedGallery(
            plan = plan,
            state = rememberGalleryState(),
            contentDescription = contentDescription,
            cellDescription = cellDescription,
            header = {},
            cell = { index, _, _ -> PhotoCell(photo = photos[index], onOpenAsset = onOpenAsset) },
        )
    }
}

/**
 * Solves the layout for the current photographs at the current width.
 *
 * Keyed on both, because either changing invalidates every row: a new set changes the aspect
 * sequence and a rotation changes what the rows justify to.
 */
@Composable
private fun rememberPhotoPlan(
    photos: ImmutableList<PersonPhoto>,
    width: Dp,
): LayoutPlan {
    val density = LocalDensity.current
    val widthPx = with(density) { width.toPx() }
    val spacingPx = with(density) { GalleryDefaults.Spacing.toPx() }
    val levels = GalleryDensityLevels.forWidth(width)
    val targetRowHeight = levels[levels.defaultIndex].targetRowHeight(widthPx, spacingPx)
    val metrics =
        GalleryDefaults.metrics(
            viewportWidth = widthPx,
            targetRowHeight = targetRowHeight,
            headerHeight = 0.dp,
            sectionSpacing = 0.dp,
        )
    return remember(photos, metrics) {
        LayoutPlanner.plan(
            aspects = FloatArray(photos.size) { photos[it].aspectRatio },
            sectionStarts = intArrayOf(0),
            metrics = metrics,
        )
    }
}

/**
 * One photograph.
 *
 * Filled with the asset's own dominant colour rather than a grey box. The colour rides inside
 * the encrypted metadata, so it is there before any blob has been fetched — which makes the
 * first paint look like the library rather than like a loading screen.
 */
@Composable
private fun PhotoCell(
    photo: PersonPhoto,
    onOpenAsset: (String) -> Unit,
) {
    val markers = remember(photo.videoDuration) { GalleryCellMarkers(videoDuration = photo.videoDuration) }
    Box(
        modifier =
            Modifier
                .fillMaxSize()
                .background(Color(OPAQUE_ALPHA or photo.placeholder))
                .clickable { onOpenAsset(photo.assetId) },
    ) {
        GalleryCellChrome(markers = markers)
    }
}
