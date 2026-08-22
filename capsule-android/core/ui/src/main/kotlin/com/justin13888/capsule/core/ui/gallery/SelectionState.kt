package com.justin13888.capsule.core.ui.gallery

import androidx.collection.MutableIntSet
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback

/**
 * Which assets are selected, and whether the gallery is in selection mode at all.
 *
 * Backed by [MutableIntSet] rather than a `Set<Int>` because "select all" on a real
 * library is a hundred thousand entries, and the boxed form is a hundred thousand
 * allocations plus a pointer chase per hit test during a drag. The set itself is not
 * observable, so every mutation bumps a revision counter and every query reads it — one
 * snapshot read per query, and recomposition stays correct without copying the set.
 *
 * Selection mode is tracked separately from emptiness. A drag that ends up selecting
 * nothing must still leave the toolbar open, and deselecting the last item must not
 * silently drop the user out of a mode they are still working in.
 */
@Stable
class SelectionState internal constructor(
    initial: IntArray = IntArray(0),
    initiallyActive: Boolean = false,
) {
    private val selected = MutableIntSet(initial.size)

    /** Bumped on every mutation. See [tracking] for why. */
    private var revision by mutableIntStateOf(0)

    /** The selection as it stood when the running drag began, so a drag extends rather than replaces. */
    private var dragBase: IntArray = IntArray(0)
    private var dragAnchor: Int = -1

    /** Whether the gallery is in selection mode. */
    var isActive: Boolean by mutableStateOf(initiallyActive)
        private set

    init {
        initial.forEach(selected::add)
    }

    /**
     * Always `true`; reading [revision] is the point.
     *
     * [MutableIntSet] is invisible to the snapshot system, so every query below is guarded
     * by this to register the read. Naming it rather than sprinkling bare reads keeps the
     * mechanism explicit — a query that forgets it silently stops recomposing.
     */
    private val tracking: Boolean get() = revision >= 0

    /** How many assets are selected. */
    val size: Int get() = if (tracking) selected.size else 0

    /** Whether [asset] is part of the current selection. */
    fun contains(asset: Int): Boolean = tracking && selected.contains(asset)

    /** Enters selection mode without selecting anything. */
    fun activate() {
        isActive = true
    }

    /** Leaves selection mode and drops the selection. */
    fun clear() {
        selected.clear()
        dragBase = IntArray(0)
        dragAnchor = -1
        isActive = false
        revision++
    }

    /** Adds or removes [asset], entering selection mode if it was not already on. */
    fun toggle(asset: Int) {
        if (asset < 0) return
        isActive = true
        if (!selected.remove(asset)) selected.add(asset)
        revision++
    }

    /** Adds every asset in `from until until`. */
    fun selectRange(
        from: Int,
        until: Int,
    ) {
        if (until <= from) return
        isActive = true
        for (asset in from until until) selected.add(asset)
        revision++
    }

    /** The selection as a sorted array, for handing to a feature's view model. */
    fun snapshot(): IntArray {
        if (!tracking) return IntArray(0)
        val out = IntArray(selected.size)
        var index = 0
        selected.forEach { out[index++] = it }
        out.sort()
        return out
    }

    /**
     * Starts a drag-select at [asset].
     *
     * The selection as it stands is copied aside so the drag paints a range on top of it:
     * a photographer who has already picked five frames and then sweeps a sixth run
     * expects six runs selected, not one.
     */
    internal fun beginDrag(asset: Int) {
        isActive = true
        dragBase = snapshot()
        dragAnchor = asset
        selected.add(asset)
        revision++
    }

    /**
     * Extends the running drag to [asset], selecting the inclusive index range between it
     * and the anchor.
     *
     * Recomputed from the anchor every time rather than accumulated, so dragging back the
     * way you came deselects — the only way an overshot sweep is recoverable without
     * lifting the finger and starting again.
     */
    internal fun dragTo(asset: Int) {
        if (dragAnchor < 0 || asset < 0) return
        selected.clear()
        dragBase.forEach(selected::add)
        val from = minOf(dragAnchor, asset)
        val until = maxOf(dragAnchor, asset) + 1
        for (index in from until until) selected.add(index)
        revision++
    }

    /** Ends the running drag. Whatever it selected stays selected. */
    internal fun endDrag() {
        dragAnchor = -1
        dragBase = IntArray(0)
    }

    /** Whether a drag-select is in flight. */
    internal val isDragging: Boolean get() = dragAnchor >= 0

    companion object {
        /** Saves the selection and the mode across a configuration change. */
        val Saver: Saver<SelectionState, List<Int>> =
            Saver(
                save = { state -> listOf(if (state.isActive) 1 else 0) + state.snapshot().toList() },
                restore = { saved ->
                    SelectionState(
                        initial = saved.drop(1).toIntArray(),
                        initiallyActive = saved.firstOrNull() == 1,
                    )
                },
            )
    }
}

/** A [SelectionState] that survives recomposition and configuration change. */
@Composable
fun rememberSelectionState(): SelectionState = rememberSaveable(saver = SelectionState.Saver) { SelectionState() }

/**
 * Long-press to enter selection mode, then drag to sweep an inclusive range.
 *
 * Hit testing goes through [LayoutPlan.assetAt] against *content* coordinates rather than
 * against composed nodes. A fast drag outruns composition — the finger is over a row that
 * has not been composed yet, and on a plan-driven gallery it may never be, because it sits
 * outside the overscan. Asking the plan instead makes the range right regardless of what
 * happens to be on screen, and makes it testable without a renderer.
 *
 * Composable because the long press earns a haptic, and the feedback handle only exists
 * inside a composition.
 *
 * @param plan the plan being rendered; hit tests resolve against it.
 * @param state supplies the scroll offset that turns a touch into a content coordinate.
 * @param selection the set to mutate.
 * @param enabled `false` leaves the pointer stream untouched, for a read-only gallery.
 */
@Composable
fun Modifier.galleryDragSelect(
    plan: LayoutPlan,
    state: GalleryState,
    selection: SelectionState,
    enabled: Boolean = true,
): Modifier {
    if (!enabled) return this
    val haptics = LocalHapticFeedback.current
    return this.pointerInput(plan, state, selection) {
        detectDragGesturesAfterLongPress(
            onDragStart = { offset ->
                val asset = plan.assetAt(offset.x, offset.y + state.scrollY)
                if (asset >= 0) {
                    haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                    selection.beginDrag(asset)
                }
            },
            onDrag = { change, _ ->
                val asset = plan.assetAt(change.position.x, change.position.y + state.scrollY)
                if (asset >= 0) selection.dragTo(asset)
            },
            onDragEnd = { selection.endDrag() },
            onDragCancel = { selection.endDrag() },
        )
    }
}
