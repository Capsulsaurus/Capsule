package com.justin13888.capsule.core.ui.gallery

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleShapes
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import kotlin.math.roundToInt

private val ThumbWidth = 40.dp
private val TrackInset = 4.dp
private val BubbleInset = 8.dp
private val TickTextInset = 6.dp

/**
 * The least vertical distance between two rail labels.
 *
 * Sized so two `labelSmall` lines cannot touch. Ticks closer than this are dropped rather
 * than drawn on top of one another.
 */
private val TickMinimumGap = 28.dp

/** How opaque the ground behind a rail label is. Enough to read against, not a band. */
private const val TICK_SCRIM_ALPHA = 0.82f

/** Vertical breathing room inside a label's scrim. */
private val TickScrimInset = 2.dp
private const val FADE_MILLIS = 180

/**
 * The right-edge date rail.
 *
 * Everything it claims is a lookup into the plan rather than an estimate: the thumb sits at
 * [GalleryScroll.fractionOf], a drag maps back through [GalleryScroll.scrollForFraction],
 * and the bubble names [GalleryScroll.sectionAt]. That symmetry is what makes the control
 * trustworthy — dragging the thumb two-thirds down and reading "August" means the library
 * really is two-thirds of the way through August there.
 *
 * It hides when nothing is happening and returns on the first scrolled pixel. A permanently
 * visible rail is permanent chrome on top of the thing the app exists to show.
 *
 * Positions are computed in the layout phase from lambdas rather than read during
 * composition, so a scroll moves the thumb without recomposing anything.
 *
 * @param plan the plan being scrolled; ticks and fractions resolve against it.
 * @param state the scroll position to read and to drive.
 * @param sectionLabel the bubble's text for a section. The gallery cannot format a date.
 * @param ticks month or year markers to draw down the rail.
 * @param autoHide `false` pins the rail open, for a pointer-driven window where it is not
 *   competing with the content for space.
 */
@Composable
fun GalleryScrubber(
    plan: LayoutPlan,
    state: GalleryState,
    sectionLabel: (section: Int) -> String,
    modifier: Modifier = Modifier,
    ticks: List<GalleryScrubberTick> = emptyList(),
    autoHide: Boolean = true,
) {
    val density = LocalDensity.current
    val haptics = LocalHapticFeedback.current
    val scope = rememberCoroutineScope()

    var trackHeight by remember { mutableFloatStateOf(0f) }
    var dragging by remember { mutableStateOf(false) }
    var visible by remember { mutableStateOf(!autoHide) }

    val thumbHeight = with(density) { GalleryDefaults.ScrubberThumbHeight.toPx() }
    val travel = (trackHeight - thumbHeight).coerceAtLeast(1f)
    val section = state.firstVisibleSection

    IdleFade(state = state, autoHide = autoHide, dragging = dragging) { visible = it }

    // A tick per day crossed is the only feedback that survives a thumb the finger is
    // covering. Fired from an effect rather than inline: the section is snapshot state read
    // during composition, and feeding it back there would be a write to what was just read.
    LaunchedEffect(section, dragging) {
        if (dragging && section >= 0) haptics.performHapticFeedback(HapticFeedbackType.SegmentTick)
    }

    val fade =
        animateFloatAsState(
            targetValue = if (visible || dragging) 1f else 0f,
            animationSpec = if (CapsuleTheme.reduceMotion) snap() else tween(FADE_MILLIS),
            label = "capsule-gallery-scrubber-fade",
        )

    val railDescription = stringResource(R.string.gallery_scrubber_a11y)
    val earlier = stringResource(R.string.gallery_scrubber_action_earlier)
    val later = stringResource(R.string.gallery_scrubber_action_later)
    val fraction = { state.scrollFraction }

    Box(
        modifier =
            modifier
                .testTag(GalleryTestTags.SCRUBBER)
                .fillMaxHeight()
                .width(GalleryDefaults.ScrubberWidth)
                .onSizeChanged { trackHeight = it.height.toFloat() }
                .graphicsLayer { alpha = fade.value }
                .semantics {
                    contentDescription = railDescription
                    customActions =
                        listOf(
                            CustomAccessibilityAction(earlier) {
                                scope.launch { state.jumpSection(plan, +1) }
                                true
                            },
                            CustomAccessibilityAction(later) {
                                scope.launch { state.jumpSection(plan, -1) }
                                true
                            },
                        )
                }.pointerInput(plan, state, travel, thumbHeight) {
                    detectVerticalDragGestures(
                        onDragStart = { offset ->
                            dragging = true
                            state.seekTo(plan, offset.y, thumbHeight, travel)
                        },
                        onVerticalDrag = { change, _ ->
                            state.seekTo(plan, change.position.y, thumbHeight, travel)
                        },
                        onDragEnd = { dragging = false },
                        onDragCancel = { dragging = false },
                    )
                },
    ) {
        ScrubberTicks(plan = plan, state = state, ticks = ticks, travel = travel, thumbHeight = thumbHeight)
        ScrubberThumb(fraction = fraction, travel = travel)
        if (dragging && section >= 0) {
            ScrubberBubble(text = sectionLabel(section), fraction = fraction, travel = travel)
        }
    }
}

/**
 * Shows the rail on movement and hides it a beat after movement stops.
 *
 * `collectLatest` rather than a `LaunchedEffect` keyed on the offset: keying on the offset
 * would restart the effect on every scrolled pixel, which is sixty coroutine launches a
 * second to run one timer.
 */
@Composable
private fun IdleFade(
    state: GalleryState,
    autoHide: Boolean,
    dragging: Boolean,
    onVisibilityChange: (Boolean) -> Unit,
) {
    LaunchedEffect(state, autoHide, dragging) {
        if (!autoHide) {
            onVisibilityChange(true)
            return@LaunchedEffect
        }
        snapshotFlow { state.scrollY }.collectLatest {
            onVisibilityChange(true)
            if (!dragging) {
                delay(GalleryDefaults.SCRUBBER_IDLE_MILLIS)
                onVisibilityChange(false)
            }
        }
    }
}

@Composable
private fun BoxScope.ScrubberThumb(
    fraction: () -> Float,
    travel: Float,
) {
    Box(
        modifier =
            Modifier
                .align(Alignment.TopEnd)
                .railOffset(fraction, travel)
                .padding(end = TrackInset)
                .width(ThumbWidth)
                .height(GalleryDefaults.ScrubberThumbHeight)
                .clip(CapsuleShapes.extraLarge)
                .background(MaterialTheme.colorScheme.secondaryContainer),
    )
}

@Composable
private fun BoxScope.ScrubberBubble(
    text: String,
    fraction: () -> Float,
    travel: Float,
) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLargeEmphasized,
        color = MaterialTheme.colorScheme.onPrimaryContainer,
        modifier =
            Modifier
                .align(Alignment.TopEnd)
                .testTag(GalleryTestTags.SCRUBBER_BUBBLE)
                .railOffset(fraction, travel)
                .padding(end = GalleryDefaults.ScrubberWidth)
                .clip(CapsuleShapes.extraLarge)
                .background(MaterialTheme.colorScheme.primaryContainer)
                .padding(horizontal = BubbleInset, vertical = BubbleInset / 2),
    )
}

@Composable
private fun BoxScope.ScrubberTicks(
    plan: LayoutPlan,
    state: GalleryState,
    ticks: List<GalleryScrubberTick>,
    travel: Float,
    thumbHeight: Float,
) {
    if (ticks.isEmpty()) return
    val viewportHeight = state.viewportHeight

    // Thin the ticks to what the rail can actually show. A five-year library offers a
    // label per month, and at a phone's height those land a few pixels apart and stack
    // into an illegible smear down the grid — a rail that names nothing while covering
    // photographs is worse than no rail. Whichever labels survive are spread evenly, so
    // the rail still reads as a map of the whole library rather than of its first months.
    val minimumGap = with(LocalDensity.current) { TickMinimumGap.toPx() }
    val placed =
        remember(plan, ticks, travel, viewportHeight, minimumGap) {
            val kept = ArrayList<Pair<GalleryScrubberTick, Float>>(ticks.size)
            var lastY = Float.NEGATIVE_INFINITY
            ticks
                .mapNotNull { tick ->
                    headerSlotOf(plan, tick.section)
                        .takeIf { it >= 0 }
                        ?.let { slot ->
                            tick to GalleryScroll.fractionOf(plan, plan.offsetOf(slot), viewportHeight)
                        }
                }.forEach { placement ->
                    val y = placement.second * travel
                    if (y - lastY >= minimumGap) {
                        lastY = y
                        kept += placement
                    }
                }
            kept
        }

    for ((tick, at) in placed) {
        Text(
            text = tick.label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            // A month name is one line. Without this the label inherits the rail's own
            // narrow width, and Compose wraps it one *character* per line — which is what
            // produced a column of stacked glyphs down the middle of the grid rather than
            // anything a reader could use. Unbounded width lets it lay out at its natural
            // size and extend left of the rail, which is where there is room for it.
            maxLines = 1,
            softWrap = false,
            modifier =
                Modifier
                    .align(Alignment.TopEnd)
                    .railOffset({ at }, travel, centreOn = thumbHeight)
                    .padding(end = TrackInset + ThumbWidth + TickTextInset)
                    .wrapContentWidth(align = Alignment.End, unbounded = true)
                    // The grid runs edge to edge, so a rail label is always over a
                    // photograph and its legibility would otherwise depend on whatever
                    // happens to be behind it. The scrim is what makes the date readable
                    // against a bright sky and a dark interior alike.
                    .clip(CapsuleShapes.small)
                    .background(MaterialTheme.colorScheme.surface.copy(alpha = TICK_SCRIM_ALPHA))
                    .padding(horizontal = TickTextInset, vertical = TickScrimInset),
        )
    }
}

/** Positions an element down the rail at a fraction of its travel, in the layout phase. */
private fun Modifier.railOffset(
    fraction: () -> Float,
    travel: Float,
    centreOn: Float = 0f,
): Modifier =
    this.offset {
        IntOffset(x = 0, y = (fraction().coerceIn(0f, 1f) * travel + centreOn / 2f).roundToInt())
    }

/**
 * Maps a touch on the rail to a scroll offset.
 *
 * Applied through [GalleryState.dispatchRawDelta] rather than an animation: the finger is
 * already the animation, and interposing a spring makes the rail feel like it is arguing.
 */
private fun GalleryState.seekTo(
    plan: LayoutPlan,
    y: Float,
    thumbHeight: Float,
    travel: Float,
) {
    val fraction = ((y - thumbHeight / 2f) / travel).coerceIn(0f, 1f)
    val target = GalleryScroll.scrollForFraction(plan, fraction, viewportHeight)
    dispatchRawDelta(target - scrollY)
}

/** Steps [delta] sections and scrolls there. Backs the rail's accessibility actions. */
private suspend fun GalleryState.jumpSection(
    plan: LayoutPlan,
    delta: Int,
) {
    val current = firstVisibleSection
    if (current < 0) return
    val next = (current + delta).coerceIn(0, plan.sectionCount - 1)
    if (next == current) return
    animateScrollToAsset(plan.sectionStart(next))
}
