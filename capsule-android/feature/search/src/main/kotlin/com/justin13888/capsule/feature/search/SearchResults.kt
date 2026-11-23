package com.justin13888.capsule.feature.search

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.ui.gallery.GalleryCellChrome
import com.justin13888.capsule.core.ui.gallery.GalleryCellMarkers
import com.justin13888.capsule.core.ui.gallery.GalleryDefaults
import com.justin13888.capsule.core.ui.gallery.GalleryDensityLevels
import com.justin13888.capsule.core.ui.gallery.GalleryHeaderInfo
import com.justin13888.capsule.core.ui.gallery.JustifiedGallery
import com.justin13888.capsule.core.ui.gallery.LayoutPlan
import com.justin13888.capsule.core.ui.gallery.LayoutPlanner
import com.justin13888.capsule.core.ui.gallery.rememberGalleryState
import kotlinx.collections.immutable.ImmutableList

/** Test handle for the result grid, which has no text of its own to address it by. */
const val SEARCH_RESULTS_TAG: String = "search.results"

private val HeaderPadding = 16.dp

/**
 * The result grid.
 *
 * The library's own justified gallery, not a second grid: results are the same photographs
 * at the same aspect ratios the timeline draws, and a uniform grid here would crop every
 * one of them differently from the screen the user just came from.
 *
 * Sections are years rather than days. A search answer is scattered across the whole
 * library, so day headers would put a header above almost every row; a year still tells the
 * user where in their history they have landed.
 */
@Composable
fun SearchResultsGrid(
    results: ImmutableList<SearchResultItem>,
    onOpenAsset: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (results.isEmpty()) return
    BoxWithConstraints(modifier = modifier.testTag(SEARCH_RESULTS_TAG)) {
        val plan = rememberResultPlan(results, maxWidth)
        val gallery = rememberGalleryState()
        val cellTemplate = stringResource(R.string.search_a11y_result)
        JustifiedGallery(
            plan = plan,
            state = gallery,
            contentDescription = stringResource(R.string.search_a11y_results),
            cellDescription = { index ->
                cellTemplate.withArguments("index" to index + 1, "count" to results.size)
            },
            header = { info -> YearHeader(results = results, plan = plan, info = info) },
            cell = { index, _, _ -> ResultCell(item = results[index], onOpenAsset = onOpenAsset) },
        )
    }
}

/**
 * Solves the layout for the current results at the current width.
 *
 * Keyed on both, because either changing invalidates every row: a new answer changes the
 * aspect sequence and a rotation changes what the rows justify to.
 */
@Composable
private fun rememberResultPlan(
    results: ImmutableList<SearchResultItem>,
    width: Dp,
): LayoutPlan {
    val density = LocalDensity.current
    val widthPx = with(density) { width.toPx() }
    val spacingPx = with(density) { GalleryDefaults.Spacing.toPx() }
    val levels = GalleryDensityLevels.forWidth(width)
    val targetRowHeight = levels[levels.defaultIndex].targetRowHeight(widthPx, spacingPx)
    val metrics = GalleryDefaults.metrics(viewportWidth = widthPx, targetRowHeight = targetRowHeight)
    return remember(results, metrics) {
        LayoutPlanner.plan(
            aspects = FloatArray(results.size) { results[it].aspectRatio },
            sectionStarts = SearchIndexing.sectionStarts(results),
            metrics = metrics,
        )
    }
}

/**
 * A year marker over the results taken in it.
 *
 * The pinned copy gets an opaque container and the in-content copy does not: the pinned one
 * sits over photographs, and a transparent label there is unreadable against half of them.
 */
@Composable
private fun YearHeader(
    results: ImmutableList<SearchResultItem>,
    plan: LayoutPlan,
    info: GalleryHeaderInfo,
) {
    val first = plan.sectionStart(info.section).coerceIn(0, results.lastIndex)
    val year = results[first].year
    val label = stringResource(R.string.search_a11y_section).withArguments("year" to year)
    Surface(
        modifier = Modifier.fillMaxWidth(),
        color = if (info.isPinned) MaterialTheme.colorScheme.surface else Color.Transparent,
    ) {
        Box(
            modifier = Modifier.fillMaxSize().padding(horizontal = HeaderPadding),
            contentAlignment = Alignment.CenterStart,
        ) {
            Text(
                text = year.toString(),
                style = MaterialTheme.typography.titleSmallEmphasized,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.capsuleDescription(label),
            )
        }
    }
}

/**
 * One result.
 *
 * Fills with the asset's own dominant colour rather than a grey box. The colour rides inside
 * the encrypted metadata, so it is available before any blob has been fetched — which makes
 * the first paint of a result grid look like the library rather than like a loading screen.
 */
@Composable
private fun ResultCell(
    item: SearchResultItem,
    onOpenAsset: (String) -> Unit,
) {
    val markers = remember(item.videoDuration) { GalleryCellMarkers(videoDuration = item.videoDuration) }
    Box(
        modifier =
            Modifier
                .fillMaxSize()
                .background(Color(OPAQUE_ALPHA or item.placeholder))
                .clickable { onOpenAsset(item.assetId) },
    ) {
        GalleryCellChrome(markers = markers)
    }
}

/** The alpha byte of an opaque `0xRRGGBB` fill. */
private const val OPAQUE_ALPHA = 0xFF000000.toInt()
