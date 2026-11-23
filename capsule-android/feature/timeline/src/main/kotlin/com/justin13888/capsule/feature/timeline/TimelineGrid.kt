package com.justin13888.capsule.feature.timeline

import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.DayBucket
import com.justin13888.capsule.core.ui.gallery.GalleryDefaults
import com.justin13888.capsule.core.ui.gallery.GalleryDensityLevels
import com.justin13888.capsule.core.ui.gallery.GalleryScroll
import com.justin13888.capsule.core.ui.gallery.GalleryScrubber
import com.justin13888.capsule.core.ui.gallery.GalleryScrubberTick
import com.justin13888.capsule.core.ui.gallery.GallerySectionHeader
import com.justin13888.capsule.core.ui.gallery.GalleryState
import com.justin13888.capsule.core.ui.gallery.JustifiedGallery
import com.justin13888.capsule.core.ui.gallery.LayoutPlan
import com.justin13888.capsule.core.ui.gallery.LayoutPlanner
import com.justin13888.capsule.core.ui.gallery.rememberGalleryZoomState
import com.justin13888.capsule.core.ui.gallery.rememberSelectionState
import kotlinx.coroutines.flow.distinctUntilChanged
import java.time.YearMonth

private val ToolbarInset = 16.dp

/**
 * The justified grid, its scrubber, and the toolbar selection puts over them.
 *
 * The layout engine is `core:ui`'s and nothing here re-implements any of it: the plan is
 * solved from this library's aspect ratios, the gallery renders the plan, and the scrubber,
 * the pinned header and drag-select all read the same plan back. What this file owns is the
 * three decisions only the feature can make — what a section header says, what a tile draws,
 * and which window the view model should read.
 */
@Composable
fun TimelineGrid(
    state: TimelineUiState.Ready,
    galleryState: GalleryState,
    callbacks: TimelineCallbacks,
    modifier: Modifier = Modifier,
) {
    val labels = TimelineDateLabels.rememberLabels()
    BoxWithConstraints(modifier = modifier) {
        val widthPx = constraints.maxWidth.toFloat()
        // A zero-width pass happens once before the first real measure. Solving against it
        // would throw, and drawing nothing for one frame is invisible.
        if (widthPx <= 0f) return@BoxWithConstraints

        val density = LocalDensity.current
        val spacingPx = with(density) { GalleryDefaults.Spacing.toPx() }
        val zoom = rememberGalleryZoomState(GalleryDensityLevels.forWidth(maxWidth))
        val metrics =
            GalleryDefaults.metrics(
                viewportWidth = widthPx,
                targetRowHeight = zoom.level.targetRowHeight(widthPx, spacingPx),
            )
        val geometry = state.geometry
        val plan =
            remember(geometry, metrics) {
                LayoutPlanner.plan(geometry.aspects, geometry.sectionStarts, metrics)
            }
        val selection = rememberSelectionState()
        val describer = rememberCellDescriber(labels)

        ReportVisibleWindow(plan = plan, galleryState = galleryState, onVisibleRange = callbacks.onVisibleRange)

        JustifiedGallery(
            plan = plan,
            state = galleryState,
            modifier = Modifier.fillMaxSize(),
            selection = selection,
            zoom = zoom,
            cellDescription = { asset -> describer.describe(state.cellAt(asset)) },
            header = { info ->
                val bucket = geometry.days.getOrNull(info.section)
                GallerySectionHeader(
                    info = info,
                    label = bucket?.let(labels::day).orEmpty(),
                    count = bucket?.count ?: 0,
                )
            },
            cell = { asset, _, _ ->
                val cell = state.cellAt(asset)
                TimelineTile(
                    cell = cell,
                    onOpen = {
                        if (selection.isActive) {
                            selection.toggle(asset)
                        } else {
                            cell?.let { callbacks.onOpenAsset(it.assetId.value) }
                        }
                    },
                    modifier = Modifier.testTag(TimelineTestTags.tile(asset)),
                )
            },
        )

        GalleryScrubber(
            plan = plan,
            state = galleryState,
            sectionLabel = { section ->
                geometry.days
                    .getOrNull(section)
                    ?.let(labels::month)
                    .orEmpty()
            },
            modifier = Modifier.align(Alignment.CenterEnd),
            ticks = remember(geometry, labels) { monthTicks(geometry.days, labels) },
        )

        if (selection.isActive) {
            SelectionToolbar(
                count = selection.size,
                isOffline = state.isOffline,
                onSelectAll = { selection.selectRange(0, geometry.assetCount) },
                onAction = { action ->
                    callbacks.onSelectionAction(action, selection.snapshot())
                    // Every action ends the mode, culling included. The batch has been
                    // handed over, and leaving the user in selection with a set that has
                    // already been acted on is how a second batch gets applied to assets
                    // that were dealt with by the first.
                    selection.clear()
                },
                onDismiss = selection::clear,
                modifier = Modifier.align(Alignment.BottomCenter).padding(ToolbarInset),
            )
        }
    }
}

/**
 * Tells the view model which assets the plan says are on screen.
 *
 * Read from the plan rather than from what happens to be composed, because a fling outruns
 * composition: the rows under the finger are known exactly a frame before they are drawn, so
 * asking the plan keeps the reads ahead of the scroll instead of chasing it.
 *
 * `snapshotFlow` with `distinctUntilChanged` rather than an effect keyed on the offset: the
 * offset changes sixty times a second and the *window* changes a handful of times, and
 * restarting an effect per scrolled pixel is sixty coroutine launches to do nothing.
 */
@Composable
private fun ReportVisibleWindow(
    plan: LayoutPlan,
    galleryState: GalleryState,
    onVisibleRange: (Int, Int) -> Unit,
) {
    val overscanPx = with(LocalDensity.current) { GalleryDefaults.Overscan.toPx() }
    LaunchedEffect(plan, galleryState, onVisibleRange) {
        snapshotFlow { visibleAssets(plan, galleryState.scrollY, galleryState.viewportHeight, overscanPx) }
            .distinctUntilChanged()
            .collect { range -> if (!range.isEmpty()) onVisibleRange(range.first, range.last + 1) }
    }
}

/**
 * The assets covered by the slots the viewport intersects.
 *
 * Headers are skipped rather than treated as content: a viewport showing only a day header
 * still has the next day's first row inside its overscan, and reporting an empty window there
 * would blank the tiles the moment the user scrolled onto a section boundary.
 */
internal fun visibleAssets(
    plan: LayoutPlan,
    scrollY: Float,
    viewportHeight: Float,
    overscan: Float,
): IntRange {
    if (plan.assetCount == 0 || viewportHeight <= 0f) return IntRange.EMPTY
    val slots = GalleryScroll.visibleSlots(plan, scrollY, viewportHeight, overscan)
    if (slots.isEmpty()) return IntRange.EMPTY
    var first = -1
    var last = -1
    for (slot in slots) {
        if (plan.isHeader(slot)) continue
        if (first < 0) first = plan.rowFrom(slot)
        last = plan.rowUntil(slot) - 1
    }
    return if (first < 0 || last < first) IntRange.EMPTY else first..last
}

/**
 * One tick per month the library covers.
 *
 * A month rather than a day, because a rail forty-eight density pixels wide cannot carry five
 * years of days and a rail that tried would be an unreadable smear. Where each tick *sits* is
 * still derived from the plan, so a month holding four thousand photographs occupies more rail
 * than one holding forty — which is the only version of this control that does not lie.
 */
internal fun monthTicks(
    days: List<DayBucket>,
    labels: TimelineDateLabels,
): List<GalleryScrubberTick> {
    val ticks = ArrayList<GalleryScrubberTick>()
    var current: YearMonth? = null
    days.forEachIndexed { section, bucket ->
        val month = YearMonth.from(bucket.day)
        if (month != current) {
            current = month
            ticks += GalleryScrubberTick(section = section, label = labels.month(bucket))
        }
    }
    return ticks
}

/**
 * What a screen reader says about one tile.
 *
 * The templates are resolved once and the substitution is plain Kotlin, because the gallery
 * asks for a cell's label from outside composition — it is a lambda on a lazy item provider,
 * not a composable slot — and because a `stringResource` per tile per frame would be a
 * resource lookup on the scroll path.
 *
 * A degraded tile folds its reason into the label rather than leaving it to a badge, which is
 * the one piece of chrome a screen reader cannot see.
 */
@Immutable
internal class TimelineCellDescriber(
    private val labels: TimelineDateLabels,
    private val photoTemplate: String,
    private val videoTemplate: String,
    private val degradedTemplate: String,
    private val reasons: Map<TimelineDegradation, String>,
) {
    /** The announced label for [cell], or `null` when its window has not been read yet. */
    fun describe(cell: TimelineCell?): String? {
        if (cell == null) return null
        val template = if (cell.isVideo) videoTemplate else photoTemplate
        val base = template.withArguments("date" to labels.announce(cell.capturedAt))
        val reason = cell.degradation?.let(reasons::get) ?: return base
        return degradedTemplate.withArguments("label" to base, "reason" to reason)
    }
}

/** Resolves every template the tile labels need, once per locale. */
@Composable
internal fun rememberCellDescriber(labels: TimelineDateLabels): TimelineCellDescriber {
    val photo = stringResource(R.string.timeline_a11y_cell_photo)
    val video = stringResource(R.string.timeline_a11y_cell_video)
    val degraded = stringResource(R.string.timeline_a11y_degraded)
    val reasons = TimelineDegradation.entries.associateWith { stringResource(it.messageRes()) }
    return remember(labels, photo, video, degraded, reasons) {
        TimelineCellDescriber(labels, photo, video, degraded, reasons)
    }
}

/** The catalog key explaining one degraded state. */
internal fun TimelineDegradation.messageRes(): Int =
    when (this) {
        TimelineDegradation.AwaitingOriginal -> R.string.timeline_degraded_awaiting_original
        TimelineDegradation.FullResolutionUnavailable -> R.string.timeline_degraded_full_resolution
        TimelineDegradation.UnreadableOnThisDevice -> R.string.timeline_degraded_unreadable
        TimelineDegradation.Quarantined -> R.string.timeline_degraded_quarantined
    }
