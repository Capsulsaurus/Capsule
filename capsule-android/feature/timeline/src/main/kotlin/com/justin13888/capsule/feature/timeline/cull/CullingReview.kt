package com.justin13888.capsule.feature.timeline.cull

import androidx.compose.foundation.background
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.NavigateBefore
import androidx.compose.material.icons.automirrored.filled.NavigateNext
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleChipState
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleFilterChipRow
import com.justin13888.capsule.core.designsystem.component.CapsuleToggleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.designsystem.component.capsuleTouchTarget
import com.justin13888.capsule.core.designsystem.theme.CapsuleShapes
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.ui.gallery.GalleryCellChrome
import com.justin13888.capsule.feature.timeline.TimelineTestTags
import com.justin13888.capsule.feature.timeline.messageRes
import com.justin13888.capsule.feature.timeline.withArguments
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.toImmutableList
import kotlin.math.abs

private val ReviewPadding = 12.dp
private val FilmstripHeight = 88.dp
private val ThumbSize = 72.dp

/** How far a drag must travel before it counts as a decision rather than a wobble. */
private const val SWIPE_THRESHOLD = 96f

/** Enough scrim that light text survives a light photograph. */
private val CaptionScrim = Color.Black.copy(alpha = 0.55f)

/** Everything the culling review can ask for. See `TimelineCallbacks` for why this is bundled. */
@Immutable
class CullingCallbacks(
    /** Leaves the review and returns to the grid. */
    val onClose: () -> Unit = {},
    /** Restricts the review to one flag. */
    val onFilter: (CullFilter) -> Unit = {},
    /** Flags the frame under review. */
    val onFlag: (CullFlag) -> Unit = {},
    /** Steps the review through the filtered set. */
    val onMove: (Int) -> Unit = {},
    /** Jumps the review to a position in the filtered set. */
    val onMoveTo: (Int) -> Unit = {},
    /** Shows or dismisses the confirmation in front of the sweep. */
    val onConfirmSweep: (Boolean) -> Unit = {},
    /** Runs the sweep. */
    val onSweep: () -> Unit = {},
)

/**
 * The culling pass: one photograph, one decision, and a way to see the rest.
 *
 * Full bleed, because the decision is about the photograph and nothing else — a review screen
 * with chrome around the frame is a review screen where the chrome gets judged too. Everything
 * below it is either a way to record a decision or a way to see what the decisions add up to.
 *
 * Two input paths, both reversible and neither confirmed. A drag flags — up is a pick, down is
 * a reject — or walks the set sideways; a keyboard does the same with the arrows, `P`, `X` and
 * `U`, because a photographer culling four hundred frames does it with one hand on a keyboard
 * and not with a thumb. Flagging is a stamped register write that touches no bytes, which is
 * why it needs no confirmation; the reject sweep, the one action that moves photographs, is
 * the one that asks.
 */
@Composable
fun CullingReview(
    state: CullingUiState,
    callbacks: CullingCallbacks,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier =
            modifier
                .fillMaxSize()
                .testTag(TimelineTestTags.REVIEW)
                .reviewKeys(callbacks)
                .focusable(),
    ) {
        CapsuleTopAppBar(
            title = stringResource(R.string.timeline_cull_title),
            subtitle = state.positionLabel(),
            navigationIcon = {
                IconButton(onClick = callbacks.onClose) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.timeline_cull_exit),
                    )
                }
            },
        )
        CapsuleFilterChipRow(
            chips = state.filterChips(callbacks.onFilter),
            modifier = Modifier.fillMaxWidth().padding(horizontal = ReviewPadding),
        )
        Box(modifier = Modifier.fillMaxWidth().weight(1f)) {
            val current = state.current
            if (current == null) {
                CapsuleEmptyState(
                    title = stringResource(R.string.timeline_cull_empty_title),
                    modifier = Modifier.fillMaxSize(),
                    body = stringResource(R.string.timeline_cull_empty_body),
                )
            } else {
                OneUp(
                    frame = current,
                    position = state.position,
                    total = state.visible.size,
                    callbacks = callbacks,
                    modifier = Modifier.fillMaxSize(),
                )
            }
        }
        ReviewControls(current = state.current?.flag, callbacks = callbacks)
        Filmstrip(state = state, onMoveTo = callbacks.onMoveTo)
        SweepFooter(state = state, callbacks = callbacks)
    }
}

/**
 * The keyboard shortcuts a culling pass is actually driven by.
 *
 * Handled on key *up* so a held key does not flag a run of frames, and returning `true` only
 * for keys that did something, so everything else still reaches the focus system.
 */
private fun Modifier.reviewKeys(callbacks: CullingCallbacks): Modifier =
    onKeyEvent { event ->
        if (event.type != KeyEventType.KeyUp) return@onKeyEvent false
        when (event.key) {
            Key.P, Key.DirectionUp -> callbacks.onFlag(CullFlag.Pick)
            Key.X, Key.DirectionDown -> callbacks.onFlag(CullFlag.Reject)
            Key.U, Key.Backspace -> callbacks.onFlag(CullFlag.Neutral)
            Key.DirectionRight -> callbacks.onMove(1)
            Key.DirectionLeft -> callbacks.onMove(-1)
            Key.Escape -> callbacks.onClose()
            else -> return@onKeyEvent false
        }
        true
    }

/**
 * The frame under review, filling everything left over.
 *
 * A plain drag detector rather than a pager: a pager would own the horizontal axis and refuse
 * the vertical one, and the two axes here mean different things. The decision is taken on
 * release rather than on distance travelled, so a hesitant drag stays recoverable — nothing is
 * recorded until the finger lifts past the threshold.
 */
@Composable
private fun OneUp(
    frame: CullFrame,
    position: Int,
    total: Int,
    callbacks: CullingCallbacks,
    modifier: Modifier = Modifier,
) {
    val flag = rememberUpdatedState(callbacks.onFlag)
    val move = rememberUpdatedState(callbacks.onMove)
    val label = frame.announcedLabel(position, total)
    Box(
        modifier =
            modifier
                .testTag(TimelineTestTags.REVIEW_FRAME)
                .background(frame.cell.placeholder)
                .semantics { contentDescription = label }
                .pointerInput(frame.asset) {
                    var travel = Offset.Zero
                    detectDragGestures(
                        onDragStart = { travel = Offset.Zero },
                        onDragEnd = { resolveSwipe(travel, flag.value, move.value) },
                        onDragCancel = { travel = Offset.Zero },
                        onDrag = { change, delta ->
                            travel += delta
                            change.consume()
                        },
                    )
                },
    ) {
        // No cell chrome here. Those markers are corner annotations sized for a hundred-dp
        // thumbnail; blown up over a full-bleed frame they are a white icon on an unknown
        // photograph with nothing behind it. What they say is said instead in the caption,
        // which carries its own scrim and therefore its own contrast.
        FrameCaption(frame = frame, modifier = Modifier.align(Alignment.BottomStart).padding(ReviewPadding))
    }
}

/** Turns a completed drag into the one decision it was closest to. */
private fun resolveSwipe(
    travel: Offset,
    onFlag: (CullFlag) -> Unit,
    onMove: (Int) -> Unit,
) {
    val horizontal = abs(travel.x)
    val vertical = abs(travel.y)
    when {
        horizontal > vertical && horizontal > SWIPE_THRESHOLD -> onMove(if (travel.x < 0) 1 else -1)
        vertical > SWIPE_THRESHOLD -> onFlag(if (travel.y < 0) CullFlag.Pick else CullFlag.Reject)
        else -> Unit
    }
}

/**
 * The rating and the degraded reason, over the frame.
 *
 * The star rating is shown *because* it is orthogonal to the flag: a photographer rejecting a
 * three-star frame is making a second, independent decision, and a review screen that hid the
 * rating would make the two look like one.
 */
@Composable
private fun FrameCaption(
    frame: CullFrame,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.clip(CapsuleShapes.medium).background(CaptionScrim).padding(ReviewPadding),
        verticalArrangement = Arrangement.spacedBy(ReviewPadding / 3),
    ) {
        Text(
            text = stringResource(R.string.timeline_cull_rating).withArguments("rating" to frame.cell.rating),
            style = CapsuleTheme.textStyles.metadataEmphasized,
            color = Color.White,
        )
        frame.cell.markers.videoDuration?.let { duration ->
            Text(
                text = stringResource(R.string.gallery_a11y_video_duration).withArguments("duration" to duration),
                style = CapsuleTheme.textStyles.metadata,
                color = Color.White,
            )
        }
        frame.cell.degradation?.let { degradation ->
            Text(
                text = stringResource(degradation.messageRes()),
                style = CapsuleTheme.textStyles.metadata,
                color = Color.White,
            )
        }
    }
}

/**
 * The three flags, always all three, with a way through the set on either side.
 *
 * Trinary and not a toggle: "undecided" is a real state a photographer returns a frame to, and
 * a two-state control would make un-rejecting something indistinguishable from picking it.
 *
 * The two arrows exist for the people the drag and the keyboard leave out. A swipe is invisible
 * to a screen reader and a hardware keyboard is not on a phone, so without a visible previous
 * and next the review would be a mode some users could enter and not get through.
 */
@Composable
private fun ReviewControls(
    current: CullFlag?,
    callbacks: CullingCallbacks,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(ReviewPadding),
        horizontalArrangement = Arrangement.spacedBy(ReviewPadding, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        IconButton(onClick = { callbacks.onMove(-1) }, modifier = Modifier.capsuleTouchTarget()) {
            Icon(
                imageVector = Icons.AutoMirrored.Filled.NavigateBefore,
                contentDescription = stringResource(R.string.timeline_cull_a11y_previous),
            )
        }
        FlagButton(CullFlag.Pick, stringResource(R.string.timeline_cull_pick), current, callbacks.onFlag)
        FlagButton(CullFlag.Neutral, stringResource(R.string.timeline_cull_neutral), current, callbacks.onFlag)
        FlagButton(CullFlag.Reject, stringResource(R.string.timeline_cull_reject), current, callbacks.onFlag)
        IconButton(onClick = { callbacks.onMove(1) }, modifier = Modifier.capsuleTouchTarget()) {
            Icon(
                imageVector = Icons.AutoMirrored.Filled.NavigateNext,
                contentDescription = stringResource(R.string.timeline_cull_a11y_next),
            )
        }
    }
}

@Composable
private fun FlagButton(
    flag: CullFlag,
    label: String,
    current: CullFlag?,
    onFlag: (CullFlag) -> Unit,
) {
    CapsuleToggleButton(
        checked = current == flag,
        onCheckedChange = { onFlag(flag) },
        label = label,
    )
}

/**
 * The rest of the review, one thumbnail at a time.
 *
 * A filmstrip and not a grid: the pass is linear, and the only spatial question a photographer
 * asks mid-review is "what came just before this one". A `LazyRow` because a review set runs to
 * hundreds, and composing all of them would cost more than the one-up above it.
 */
@Composable
private fun Filmstrip(
    state: CullingUiState,
    onMoveTo: (Int) -> Unit,
) {
    val stripLabel = stringResource(R.string.timeline_cull_a11y_filmstrip)
    LazyRow(
        modifier =
            Modifier
                .fillMaxWidth()
                .height(FilmstripHeight)
                .testTag(TimelineTestTags.REVIEW_FILMSTRIP)
                .semantics { contentDescription = stripLabel },
        horizontalArrangement = Arrangement.spacedBy(ReviewPadding / 2),
        contentPadding = PaddingValues(horizontal = ReviewPadding),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        itemsIndexed(state.visible, key = { _, frame -> frame.asset }) { position, frame ->
            FilmstripThumb(
                frame = frame,
                position = position,
                total = state.visible.size,
                isCurrent = position == state.position,
                onClick = { onMoveTo(position) },
            )
        }
    }
}

@Composable
private fun FilmstripThumb(
    frame: CullFrame,
    position: Int,
    total: Int,
    isCurrent: Boolean,
    onClick: () -> Unit,
) {
    CapsuleCard(
        modifier = Modifier.size(ThumbSize).testTag(TimelineTestTags.reviewFrame(position)),
        onClick = onClick,
        selected = isCurrent,
        contentDescription = frame.announcedLabel(position, total),
    ) {
        Box(modifier = Modifier.fillMaxSize().background(frame.cell.placeholder)) {
            GalleryCellChrome(markers = frame.cell.markers)
        }
    }
}

/**
 * The one destructive action in the review, and the confirmation in front of it.
 *
 * Confirmed inline rather than in a dialog, so the counts the decision rests on stay on screen
 * while it is being made. The copy says outright that the sweep is reversible — it is a soft
 * delete under a signed retention window — because a batch action whose reversibility is
 * unclear is one users learn not to press at all.
 */
@Composable
private fun SweepFooter(
    state: CullingUiState,
    callbacks: CullingCallbacks,
) {
    val rejects = state.counts.rejects
    if (state.isConfirmingSweep) {
        SweepConfirmation(rejects = rejects, callbacks = callbacks)
        return
    }
    Row(
        modifier = Modifier.fillMaxWidth().padding(ReviewPadding),
        horizontalArrangement = Arrangement.End,
    ) {
        CapsuleButton(
            onClick = { callbacks.onConfirmSweep(true) },
            label = stringResource(R.string.timeline_cull_sweep).withArguments("count" to rejects),
            emphasis = CapsuleButtonEmphasis.Destructive,
            enabled = rejects > 0,
        )
    }
}

@Composable
private fun SweepConfirmation(
    rejects: Int,
    callbacks: CullingCallbacks,
) {
    CapsuleCard(
        modifier =
            Modifier
                .fillMaxWidth()
                .padding(ReviewPadding)
                .testTag(TimelineTestTags.REVIEW_SWEEP_CONFIRM),
    ) {
        Column(
            modifier = Modifier.padding(ReviewPadding),
            verticalArrangement = Arrangement.spacedBy(ReviewPadding),
        ) {
            Text(
                text = stringResource(R.string.timeline_cull_sweep_confirm_title),
                style = MaterialTheme.typography.titleMediumEmphasized,
            )
            Text(
                text =
                    stringResource(R.string.timeline_cull_sweep_confirm_body)
                        .withArguments("count" to rejects),
                style = MaterialTheme.typography.bodyMedium,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(ReviewPadding)) {
                CapsuleButton(
                    onClick = { callbacks.onConfirmSweep(false) },
                    label = stringResource(R.string.common_cancel),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                )
                CapsuleButton(
                    onClick = callbacks.onSweep,
                    label = stringResource(R.string.timeline_cull_sweep_confirm_action),
                    emphasis = CapsuleButtonEmphasis.Destructive,
                )
            }
        }
    }
}

/** "3 of 47" — how far through the filtered set the one-up is. */
@Composable
private fun CullingUiState.positionLabel(): String =
    stringResource(R.string.timeline_cull_position)
        .withArguments(
            "position" to (position + 1).coerceAtMost(visible.size),
            "total" to visible.size,
        )

/** The four filter chips, each carrying the count it would show. */
@Composable
private fun CullingUiState.filterChips(onFilter: (CullFilter) -> Unit): ImmutableList<CapsuleChipState> =
    listOf(
        chip(CullFilter.All, stringResource(R.string.timeline_cull_filter_all), counts.total, onFilter),
        chip(CullFilter.Picks, stringResource(R.string.timeline_cull_filter_picks), counts.picks, onFilter),
        chip(
            CullFilter.Undecided,
            stringResource(R.string.timeline_cull_filter_neutral),
            counts.undecided,
            onFilter,
        ),
        chip(CullFilter.Rejects, stringResource(R.string.timeline_cull_filter_rejects), counts.rejects, onFilter),
    ).toImmutableList()

/**
 * One chip, labelled with its own count.
 *
 * The count is part of the label rather than a badge beside it because the chips *are* the
 * review's tally: "Rejects (12)" is the number the sweep button is about to act on, and
 * splitting the two apart is how a user ends up surprised by what a batch covered.
 */
private fun CullingUiState.chip(
    target: CullFilter,
    label: String,
    count: Int,
    onFilter: (CullFilter) -> Unit,
) = CapsuleChipState(
    label = "$label ($count)",
    selected = filter == target,
    onSelectedChange = { onFilter(target) },
)

/** The announced label for one frame — where it sits, and what has been decided about it. */
@Composable
private fun CullFrame.announcedLabel(
    position: Int,
    total: Int,
): String {
    val flagLabel = stringResource(flag.labelRes())
    return stringResource(R.string.timeline_cull_a11y_frame)
        .withArguments("position" to position + 1, "total" to total, "flag" to flagLabel)
}

/** The catalog key naming one flag. An unknown token reads as undecided but is never swept. */
internal fun CullFlag?.labelRes(): Int =
    when (this) {
        CullFlag.Pick -> R.string.timeline_cull_pick
        CullFlag.Reject -> R.string.timeline_cull_reject
        CullFlag.Neutral, null -> R.string.timeline_cull_neutral
    }
