package com.justin13888.capsule.core.ui.gallery

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.scrollable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.lazy.layout.LazyLayout
import androidx.compose.foundation.lazy.layout.LazyLayoutItemProvider
import androidx.compose.foundation.lazy.layout.LazyLayoutMeasurePolicy
import androidx.compose.foundation.lazy.layout.LazyLayoutMeasureScope
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialShapes
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.State
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.MeasureResult
import androidx.compose.ui.layout.Placeable
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CollectionInfo
import androidx.compose.ui.semantics.CollectionItemInfo
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.collectionInfo
import androidx.compose.ui.semantics.collectionItemInfo
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.designsystem.theme.morphOnSelection
import com.justin13888.capsule.core.i18n.R
import kotlin.math.min
import kotlin.math.roundToInt

/** The announced strings a cell needs, resolved once rather than per cell. */
@Immutable
private data class GalleryCellLabels(
    val selected: String,
    val notSelected: String,
    val select: String,
    val deselect: String,
)

/**
 * Where one cell sits inside its row, and how big the plan solved it.
 *
 * Grouped because the four travel together from the row's measure into both the cell's
 * semantics and the caller's content, and because a screen reader's idea of a grid position
 * and a thumbnail loader's idea of a size are the same fact seen twice.
 */
@Immutable
private data class GalleryCellPlacement(
    val rowIndex: Int,
    val columnIndex: Int,
    val width: Dp,
    val height: Dp,
)

/**
 * The timeline: an infinite justified photo grid driven by a solved [LayoutPlan].
 *
 * Built on [LazyLayout] rather than `LazyColumn` because every piece of chrome around this
 * grid interrogates the plan directly. The scrubber needs a pixel-exact fraction of the
 * whole library, zoom anchoring needs to know which asset is under the fingers and where
 * it lands in a *different* plan, the sticky header needs the section at the viewport top,
 * and drag-select needs to hit-test rows that were never composed. A lazy list owns its
 * own item geometry and estimates the rest; here the geometry is already known exactly, so
 * the list abstraction would be a layer to fight rather than a layer to use.
 *
 * Only [GalleryScroll.visibleSlots] plus [overscan] is ever composed. A slot is a header or
 * a whole justified row; the row places its own assets left to right through
 * [LayoutPlan.assetX] and [LayoutPlan.assetWidth], which keeps the top-level measure pass
 * to one placement per row rather than one per photograph.
 *
 * @param plan the solved layout. Re-solving it — a zoom, a rotation, a library change — is
 *   the caller's job; the gallery notices the new plan and restores the anchored asset.
 * @param state scroll position and everything derived from it.
 * @param selection when non-null, long-press enters selection mode and drag sweeps a range.
 * @param zoom when non-null, pinch scales the grid and release asks for a new density.
 *   The caller is expected to observe [GalleryZoomState.level] and re-plan.
 * @param overscan how far beyond the viewport rows are composed.
 * @param contentDescription what a screen reader calls the grid as a whole.
 * @param cellDescription the announced label for one asset. The gallery cannot know it —
 *   it has no access to dates or captions — so the caller supplies it per index.
 * @param header draws one section header. Called for the pinned copy as well as the
 *   in-content one; see [GalleryHeaderInfo].
 * @param cell draws one asset at the size the plan solved for it. The size is passed so a
 *   caller can request a thumbnail resolution rather than measuring after the fact.
 *
 * The parameter list is long, and deliberately not folded into a configuration object:
 * every entry is a decision only the feature can make, and bundling them would trade a
 * readable call site for an unreadable one. Compose's own lazy layouts have the same shape.
 */
@Suppress("LongParameterList")
@Composable
fun JustifiedGallery(
    plan: LayoutPlan,
    state: GalleryState,
    modifier: Modifier = Modifier,
    selection: SelectionState? = null,
    zoom: GalleryZoomState? = null,
    overscan: Dp = GalleryDefaults.Overscan,
    contentDescription: String = stringResource(R.string.gallery_a11y_grid),
    cellDescription: (asset: Int) -> String? = { null },
    header: @Composable (GalleryHeaderInfo) -> Unit,
    cell: @Composable (asset: Int, width: Dp, height: Dp) -> Unit,
) {
    val overscanPx = with(LocalDensity.current) { overscan.toPx() }
    val labels =
        GalleryCellLabels(
            selected = stringResource(R.string.gallery_a11y_selected),
            notSelected = stringResource(R.string.gallery_a11y_not_selected),
            select = stringResource(R.string.gallery_action_select),
            deselect = stringResource(R.string.gallery_action_deselect),
        )

    // The content lambdas change identity on every recomposition of the caller, but the
    // item provider must not: recreating it drops every composed item and re-runs the
    // whole visible window. Latched instead, so a new lambda is picked up on the next
    // item composition without invalidating the provider.
    val latestHeader = rememberUpdatedState(header)
    val latestCell = rememberUpdatedState(cell)
    val latestDescription = rememberUpdatedState(cellDescription)

    val itemProvider =
        remember(plan, selection, labels) {
            GallerySlotItemProvider(plan, selection, labels, latestHeader, latestCell, latestDescription)
        }
    val measurePolicy = remember(plan, state, overscanPx) { GalleryMeasurePolicy(plan, state, overscanPx) }

    Box(
        modifier =
            modifier
                .testTag(GalleryTestTags.GRID)
                .clipToBounds()
                .semantics {
                    this.contentDescription = contentDescription
                    collectionInfo = CollectionInfo(rowCount = plan.slotCount, columnCount = -1)
                },
    ) {
        LazyLayout(
            itemProvider = { itemProvider },
            modifier = galleryGestureModifier(plan, state, selection, zoom),
            measurePolicy = measurePolicy,
        )
        StickyHeader(plan = plan, state = state, header = latestHeader.value)
    }
}

/**
 * The gesture stack, in the order the pointer stream has to see it.
 *
 * `graphicsLayer` sits outermost so the pinch scales a grid that is already composed —
 * a GPU transform, not a re-layout. Scrolling comes next, and the two gestures that steal
 * from it come after: they are declared last so they are treated as the inner node and get
 * the main pass first, which is what lets a long press beat a scroll that has not moved
 * and a pinch beat one that has.
 */
@Composable
private fun galleryGestureModifier(
    plan: LayoutPlan,
    state: GalleryState,
    selection: SelectionState?,
    zoom: GalleryZoomState?,
): Modifier {
    var modifier: Modifier = Modifier.fillMaxSize()
    if (zoom != null) {
        modifier =
            modifier.graphicsLayer {
                scaleX = zoom.gestureScale
                scaleY = zoom.gestureScale
            }
    }
    // reverseDirection: dragging down must reveal earlier photographs, which means a
    // positive raw delta has to *decrease* the offset. Same convention as verticalScroll.
    modifier = modifier.scrollable(state = state, orientation = Orientation.Vertical, reverseDirection = true)
    if (zoom != null) modifier = modifier.galleryZoom(state, zoom)
    if (selection != null) modifier = modifier.galleryDragSelect(plan, state, selection)
    return modifier
}

/**
 * The header of the section at the viewport top, pinned over the scrolling content.
 *
 * Drawn as an overlay rather than by re-placing the header item inside the layout. The
 * in-content copy of this same header is always at or above the viewport top — the section
 * at the top is by definition the one whose header has already passed — so the overlay
 * covers it rather than duplicating anything visible, and in exchange `isPinned` is
 * trivially correct instead of being a piece of state written during measure and read
 * during composition.
 *
 * The offset is computed in the layout phase, so travelling through a section costs a
 * placement rather than a recomposition.
 */
@Composable
private fun BoxScope.StickyHeader(
    plan: LayoutPlan,
    state: GalleryState,
    header: @Composable (GalleryHeaderInfo) -> Unit,
) {
    val section = state.firstVisibleSection
    if (section < 0 || section >= plan.sectionCount) return
    Box(
        modifier =
            Modifier
                .align(Alignment.TopStart)
                .fillMaxWidth()
                .testTag(GalleryTestTags.STICKY_HEADER)
                .offset { IntOffset(0, stickyHeaderOffset(plan, section, state.scrollY)) },
    ) {
        header(GalleryHeaderInfo(section = section, isPinned = true, isCollapsed = state.isScrolling))
    }
}

/**
 * How far the pinned header is pushed off the top by the next section arriving.
 *
 * Zero for almost the whole of a section, then negative for exactly the height of a header
 * as the next one shoulders it out. That hand-off is the only moment the pin is visible as
 * a mechanism rather than as a label.
 */
private fun stickyHeaderOffset(
    plan: LayoutPlan,
    section: Int,
    scrollY: Float,
): Int {
    val headerSlot = headerSlotOf(plan, section)
    if (headerSlot < 0) return 0
    val height = plan.heightOf(headerSlot)
    val nextSlot = headerSlotOf(plan, section + 1)
    if (nextSlot < 0) return 0
    return min(0f, plan.offsetOf(nextSlot) - scrollY - height).roundToInt()
}

/**
 * The slot holding the header of [section], or -1 when there is none.
 *
 * The planner emits a header immediately before its section's first row, so the header is
 * the slot before the one holding the section's first asset. Derived rather than stored so
 * the plan stays two flat arrays.
 */
internal fun headerSlotOf(
    plan: LayoutPlan,
    section: Int,
): Int {
    if (section < 0 || section >= plan.sectionCount) return -1
    val firstAsset = plan.sectionStart(section)
    if (firstAsset >= plan.assetCount) return -1
    val rowSlot = plan.slotOfAsset(firstAsset)
    return if (rowSlot <= 0) -1 else rowSlot - 1
}

/**
 * Composes one slot on demand.
 *
 * Keys are derived from content rather than from the slot index — the section for a
 * header, the first asset for a row — so that a re-plan at a different density keeps the
 * identity of anything that was already on screen instead of tearing every item down.
 */
private class GallerySlotItemProvider(
    private val plan: LayoutPlan,
    private val selection: SelectionState?,
    private val labels: GalleryCellLabels,
    private val header: State<@Composable (GalleryHeaderInfo) -> Unit>,
    private val cell: State<@Composable (Int, Dp, Dp) -> Unit>,
    private val cellDescription: State<(Int) -> String?>,
) : LazyLayoutItemProvider {
    override val itemCount: Int get() = plan.slotCount

    override fun getKey(index: Int): Any {
        if (plan.isHeader(index)) return "header:${plan.sectionOf(index)}"
        return "row:${plan.rowFrom(index)}"
    }

    override fun getContentType(index: Int): Any = if (plan.isHeader(index)) HEADER_TYPE else ROW_TYPE

    @Composable
    override fun Item(
        index: Int,
        key: Any,
    ) {
        if (plan.isHeader(index)) {
            header.value(GalleryHeaderInfo(section = plan.sectionOf(index), isPinned = false, isCollapsed = false))
        } else {
            GalleryRow(
                plan = plan,
                slot = index,
                selection = selection,
                labels = labels,
                cellDescription = cellDescription.value,
                cell = cell.value,
            )
        }
    }

    private companion object {
        const val HEADER_TYPE = "capsule.gallery.header"
        const val ROW_TYPE = "capsule.gallery.row"
    }
}

/**
 * One justified row.
 *
 * Laid out by hand rather than with a `Row`: the widths are already solved and must be
 * used verbatim, because they are what makes the row fill the viewport exactly. Letting a
 * flex arrangement redistribute rounding would put a one-pixel ragged edge down the side
 * of the whole library.
 */
@Composable
private fun GalleryRow(
    plan: LayoutPlan,
    slot: Int,
    selection: SelectionState?,
    labels: GalleryCellLabels,
    cellDescription: (Int) -> String?,
    cell: @Composable (Int, Dp, Dp) -> Unit,
) {
    val density = LocalDensity.current
    val from = plan.rowFrom(slot)
    val until = plan.rowUntil(slot)
    val rowHeight = plan.rowHeight(slot)
    Layout(
        content = {
            for (asset in from until until) {
                GalleryCell(
                    asset = asset,
                    placement =
                        GalleryCellPlacement(
                            rowIndex = slot,
                            columnIndex = asset - from,
                            width = with(density) { plan.assetWidth(slot, asset).toDp() },
                            height = with(density) { rowHeight.toDp() },
                        ),
                    description = cellDescription(asset),
                    selection = selection,
                    labels = labels,
                    content = cell,
                )
            }
        },
    ) { measurables, constraints ->
        val height = rowHeight.roundToInt().coerceAtLeast(0)
        val placeables =
            measurables.mapIndexed { position, measurable ->
                val width = plan.assetWidth(slot, from + position).roundToInt().coerceAtLeast(0)
                measurable.measure(Constraints.fixed(width, height))
            }
        layout(constraints.maxWidth, height) {
            placeables.forEachIndexed { position, placeable ->
                placeable.place(plan.assetX(slot, from + position).roundToInt(), 0)
            }
        }
    }
}

/**
 * One asset, plus the selection affordance and the semantics TalkBack navigates by.
 *
 * The morph is applied only while selection mode is on. It rebuilds an outline path every
 * time it is asked for, which is affordable for the dozen cells a selection gesture is
 * touching and is not affordable for the several hundred on screen during an ordinary
 * scroll — so an unselectable grid gets a plain rounded clip and no per-frame path work.
 */
@Composable
private fun GalleryCell(
    asset: Int,
    placement: GalleryCellPlacement,
    description: String?,
    selection: SelectionState?,
    labels: GalleryCellLabels,
    content: @Composable (Int, Dp, Dp) -> Unit,
) {
    val selecting = selection != null && selection.isActive
    val isSelected = selecting && selection.contains(asset)
    val shaped =
        if (selecting) {
            Modifier.morphOnSelection(isSelected = isSelected, unselected = MaterialShapes.Square)
        } else {
            Modifier.clip(RoundedCornerShape(GalleryDefaults.CellCorner))
        }
    val tint = CapsuleTheme.colors.selected.color
    Box(
        modifier =
            Modifier
                .fillMaxSize()
                .then(shaped)
                .semantics {
                    collectionItemInfo =
                        CollectionItemInfo(
                            rowIndex = placement.rowIndex,
                            rowSpan = 1,
                            columnIndex = placement.columnIndex,
                            columnSpan = 1,
                        )
                    if (description != null) contentDescription = description
                    if (selection != null && selecting) {
                        selected = isSelected
                        stateDescription = if (isSelected) labels.selected else labels.notSelected
                        customActions =
                            listOf(
                                CustomAccessibilityAction(if (isSelected) labels.deselect else labels.select) {
                                    selection.toggle(asset)
                                    true
                                },
                            )
                    }
                },
    ) {
        content(asset, placement.width, placement.height)
        if (isSelected) {
            Box(Modifier.fillMaxSize().background(tint.copy(alpha = SELECTION_TINT_ALPHA)))
        }
    }
}

/** Enough tint to read a selection across a busy photograph without hiding it. */
private const val SELECTION_TINT_ALPHA = 0.32f

/**
 * Measures and places the visible window.
 *
 * The whole pass is two binary searches and a walk: [GalleryScroll.visibleSlots] resolves
 * where the viewport starts, and everything after it is arithmetic on arrays the plan
 * already holds. That is the property that makes the cost of a scroll independent of the
 * size of the library.
 */
private class GalleryMeasurePolicy(
    private val plan: LayoutPlan,
    private val state: GalleryState,
    private val overscan: Float,
) : LazyLayoutMeasurePolicy {
    override fun LazyLayoutMeasureScope.measure(constraints: Constraints): MeasureResult {
        val width = constraints.maxWidth
        // An unbounded height means the gallery has been put inside another scroller,
        // which it cannot support: it would have to compose the entire library to report a
        // height. Measuring as zero-height makes that a visible bug rather than a hang.
        val height = if (constraints.hasBoundedHeight) constraints.maxHeight else 0
        state.onMeasured(plan, height.toFloat())

        val scrollY = state.scrollY
        val tops = ArrayList<Int>()
        val rows = ArrayList<List<Placeable>>()
        for (slot in GalleryScroll.visibleSlots(plan, scrollY, height.toFloat(), overscan)) {
            val slotHeight = plan.heightOf(slot).roundToInt().coerceAtLeast(0)
            val slotConstraints = Constraints.fixed(width, slotHeight)
            rows += compose(slot).map { it.measure(slotConstraints) }
            tops += (plan.offsetOf(slot) - scrollY).roundToInt()
        }

        return layout(width, height) {
            for (index in rows.indices) {
                val top = tops[index]
                rows[index].forEach { it.place(0, top) }
            }
        }
    }
}
