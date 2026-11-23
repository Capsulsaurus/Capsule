package com.justin13888.capsule.feature.albums.ui.common

import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.ui.gallery.GalleryMetrics
import com.justin13888.capsule.core.ui.gallery.JustifiedGallery
import com.justin13888.capsule.core.ui.gallery.LayoutPlanner
import com.justin13888.capsule.core.ui.gallery.rememberGalleryState
import kotlinx.collections.immutable.ImmutableList

/**
 * A listing's contents, laid out by the shared justified-gallery engine.
 *
 * Deliberately a thin wrapper and not a second grid. The engine already solves rows exactly,
 * anchors the scroll across a re-plan, and carries the collection semantics a screen reader
 * needs; a `LazyVerticalGrid` here would look similar, behave differently from the timeline,
 * and quietly lose all three.
 *
 * One section with a zero-height header rather than day buckets: an album's contents are a
 * set the user assembled, so date dividers would impose an order the user did not choose.
 * The timeline is where dates group photographs.
 */
@Composable
internal fun AlbumsGallery(
    assets: ImmutableList<Asset>,
    contentDescription: String,
    cellDescription: (Asset) -> String,
    modifier: Modifier = Modifier,
    onOpen: (AssetId) -> Unit = {},
) {
    val density = LocalDensity.current
    BoxWithConstraints(modifier = modifier.fillMaxSize()) {
        val widthPx = with(density) { maxWidth.toPx() }
        val plan =
            remember(assets, widthPx) {
                LayoutPlanner.plan(
                    aspects = FloatArray(assets.size) { assets[it].dimensions?.aspectRatio ?: 1f },
                    sectionStarts = intArrayOf(0),
                    metrics =
                        GalleryMetrics(
                            viewportWidth = widthPx.coerceAtLeast(1f),
                            targetRowHeight = with(density) { TARGET_ROW_HEIGHT.toPx() },
                            spacing = with(density) { SPACING.toPx() },
                            headerHeight = 0f,
                            sectionSpacing = 0f,
                        ),
                )
            }
        val state = rememberGalleryState()
        JustifiedGallery(
            plan = plan,
            state = state,
            contentDescription = contentDescription,
            cellDescription = { index -> assets.getOrNull(index)?.let(cellDescription) },
            header = {},
            cell = { index, _, _ ->
                val asset = assets[index]
                AssetThumbnail(
                    asset = asset,
                    contentDescription = cellDescription(asset),
                    markers = asset.galleryMarkers(),
                    onClick = { onOpen(asset.id) },
                )
            },
        )
    }
}

/** The row height an album grid aims for — a comfortable three-across on a phone. */
private val TARGET_ROW_HEIGHT = 132.dp

/** The gap between adjacent cells, matching the timeline's. */
private val SPACING = 2.dp
