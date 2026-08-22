package com.justin13888.capsule.shell

import androidx.compose.material3.adaptive.Posture
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfoV2
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ProvidableCompositionLocal
import androidx.compose.runtime.Stable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.geometry.Rect

/**
 * How the device is folded, reduced to the three cases a layout can act on.
 *
 * The shell derives this once and publishes it through [LocalFoldPosture]. That is the whole
 * point of the type: `Posture` from the adaptive library carries a raw hinge list, and if
 * every screen re-derived "am I in tabletop" from it they would each invent a slightly
 * different rule, and the viewer and the album grid would disagree about where the hinge is
 * on the same device.
 *
 * The underlying signal comes from `WindowInfoTracker` — the adaptive library collects
 * `FoldingFeature`s from it and folds them into `Posture`.
 */
@Stable
sealed interface FoldPosture {
    /** No separating hinge worth avoiding: a phone, a tablet, or a fully-flat foldable. */
    data object Flat : FoldPosture

    /**
     * Half-opened about a horizontal hinge, sitting on a table.
     *
     * The window is split into an upper half the user looks at and a lower half they can
     * reach. Content goes above [hinge], controls below it.
     */
    data class Tabletop(
        /** The hinge in window coordinates. */
        val hinge: Rect,
    ) : FoldPosture

    /**
     * Held like a book about a vertical hinge.
     *
     * Nothing readable may straddle [hinge]; a pane scaffold should place its split there
     * rather than wherever the width happened to land.
     */
    data class Book(
        /** The hinge in window coordinates. */
        val hinge: Rect,
    ) : FoldPosture

    /** The hinge to keep content clear of, if there is one. */
    val hingeBounds: Rect?
        get() =
            when (this) {
                is Tabletop -> hinge
                is Book -> hinge
                Flat -> null
            }
}

/**
 * Reduces an adaptive [Posture] to a [FoldPosture].
 *
 * Kept as a pure function, separate from the composable that reads the tracker, so the
 * classification is testable without a device, an emulator or a fake `WindowInfoTracker`.
 *
 * Tabletop wins over book when both could apply: `Posture.isTabletop` already means a
 * half-opened *horizontal* hinge, and a device reporting both a horizontal half-open hinge
 * and a separating vertical one is a dual-hinge device where the horizontal split is the one
 * the user's hands are working around.
 */
fun foldPostureOf(posture: Posture): FoldPosture {
    val horizontal = posture.hingeList.firstOrNull { !it.isVertical && it.isSeparating }
    if (posture.isTabletop) {
        return FoldPosture.Tabletop(horizontal?.bounds ?: Rect.Zero)
    }
    val vertical = posture.hingeList.firstOrNull { it.isVertical && it.isSeparating && !it.isFlat }
    return vertical?.let { FoldPosture.Book(it.bounds) } ?: FoldPosture.Flat
}

/** Reads the current fold posture straight from the window tracker. */
@Composable
fun currentFoldPosture(): FoldPosture = foldPostureOf(currentWindowAdaptiveInfoV2().windowPosture)

/**
 * The fold posture the shell resolved for this window.
 *
 * Defaults to [FoldPosture.Flat] rather than throwing, because a preview or a component test
 * that renders one screen in isolation is not wrong — it just has no hinge.
 */
val LocalFoldPosture: ProvidableCompositionLocal<FoldPosture> =
    compositionLocalOf<FoldPosture> { FoldPosture.Flat }
