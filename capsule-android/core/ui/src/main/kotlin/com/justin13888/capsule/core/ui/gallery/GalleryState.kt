package com.justin13888.capsule.core.ui.gallery

import androidx.compose.animation.core.AnimationSpec
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.spring
import androidx.compose.foundation.MutatePriority
import androidx.compose.foundation.gestures.ScrollScope
import androidx.compose.foundation.gestures.ScrollableState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue

/**
 * Where the timeline is scrolled to, and everything that follows from it.
 *
 * A [ScrollableState] rather than a `LazyListState` because the gallery's item space is
 * not a list of equal things: the scrubber, the sticky header and zoom anchoring all need
 * an *exact* pixel offset into a plan that already knows the height of every row in the
 * library. Once the plan is exact, scroll position is just a float, and everything the
 * chrome asks about it is a lookup in [GalleryScroll].
 *
 * The pixel offset is deliberately not what gets saved. Across a configuration change the
 * viewport width usually changes, which re-solves the plan and moves every row; restoring
 * a pixel offset would land the user somewhere they have never been. So what survives is
 * the [GalleryScroll.Anchor] — an asset and where it sat — and the offset is recomputed
 * against the new plan on the first measure.
 */
@Stable
class GalleryState internal constructor(
    initialAnchor: GalleryScroll.Anchor? = null,
) : ScrollableState {
    private var scrollYState by mutableFloatStateOf(0f)
    private var pendingAnchor: GalleryScroll.Anchor? = initialAnchor

    /**
     * The plan currently being rendered, or `null` before the first measure.
     *
     * Written from the measure pass, not from composition: the plan is a parameter of
     * [JustifiedGallery], and the state only mirrors it so that scroll clamping and
     * [animateScrollToAsset] can work without the caller threading it through.
     */
    var plan: LayoutPlan? by mutableStateOf(null)
        private set

    /** The viewport height in pixels the plan is being scrolled inside. */
    var viewportHeight: Float by mutableFloatStateOf(0f)
        private set

    /** How far the content is scrolled, in pixels from the top of the plan. */
    val scrollY: Float get() = scrollYState

    /** The furthest this plan can scroll while keeping content on screen. */
    val maxScroll: Float get() = plan?.let { GalleryScroll.maxScroll(it, viewportHeight) } ?: 0f

    /**
     * The section whose content is at the top of the viewport, or -1 before the first
     * measure.
     *
     * Derived so that reading it recomposes on a section change rather than on every
     * scrolled pixel — the difference between a header that updates and a header that
     * recomposes sixty times a second.
     */
    val firstVisibleSection: Int by derivedStateOf {
        plan?.let { GalleryScroll.sectionAt(it, scrollYState) } ?: -1
    }

    /** How far through the library the viewport is, 0 to 1. Drives the scrubber thumb. */
    val scrollFraction: Float by derivedStateOf {
        plan?.let { GalleryScroll.fractionOf(it, scrollYState, viewportHeight) } ?: 0f
    }

    /**
     * Whether a scroll is in flight — a drag, a fling, or a programmatic animation.
     *
     * The sticky header collapses on this: a full-width date bar is worth its space when
     * you are reading, and is in the way when you are travelling.
     */
    val isScrolling: Boolean get() = isScrollInProgress

    private val scrollableState = ScrollableState(::consumeScroll)

    override val isScrollInProgress: Boolean get() = scrollableState.isScrollInProgress

    override fun dispatchRawDelta(delta: Float): Float = scrollableState.dispatchRawDelta(delta)

    private fun consumeScroll(delta: Float): Float {
        val target = (scrollYState + delta).coerceIn(0f, maxScroll)
        val consumed = target - scrollYState
        scrollYState = target
        return consumed
    }

    /**
     * Records the measure pass's plan and viewport, restoring a pending anchor when the
     * plan has been re-solved.
     *
     * This runs in measure rather than composition on purpose. Re-planning and scrolling
     * are the same event to the user — a pinch changes the layout *and* the position — and
     * splitting them across two frames is exactly the flicker the anchor exists to avoid.
     */
    internal fun onMeasured(
        plan: LayoutPlan,
        viewportHeight: Float,
    ) {
        val replanned = this.plan !== plan
        if (replanned) this.plan = plan
        if (this.viewportHeight != viewportHeight) this.viewportHeight = viewportHeight

        val anchor = pendingAnchor
        val target =
            if (replanned && anchor != null) {
                pendingAnchor = null
                GalleryScroll.applyAnchor(plan, anchor, viewportHeight)
            } else {
                scrollYState.coerceIn(0f, GalleryScroll.maxScroll(plan, viewportHeight))
            }
        if (target != scrollYState) scrollYState = target
    }

    /**
     * Remember the asset at the centre of attention so the next plan can be scrolled to
     * put it back.
     *
     * Called immediately before asking the caller to re-plan — on a pinch release, on a
     * double tap, and by the saver on the way into a configuration change.
     */
    fun captureAnchor(): GalleryScroll.Anchor? {
        val plan = plan ?: return null
        val anchor = GalleryScroll.captureAnchor(plan, scrollYState, viewportHeight)
        if (anchor != null) pendingAnchor = anchor
        return anchor
    }

    /** Scroll so [asset] sits at the top of the viewport, without animating. */
    suspend fun scrollToAsset(asset: Int) {
        val plan = plan ?: return
        scroll { scrollBy(plan.offsetOfAsset(asset, viewportHeight) - scrollYState) }
    }

    /**
     * Animate until [asset] sits at the top of the viewport.
     *
     * Driven through [scroll] rather than by writing the offset directly so that a user
     * touch interrupts it: a scroll animation that cannot be grabbed feels like a hang.
     */
    suspend fun animateScrollToAsset(
        asset: Int,
        animationSpec: AnimationSpec<Float> = spring(),
    ) {
        val plan = plan ?: return
        val target = plan.offsetOfAsset(asset, viewportHeight)
        animateScrollTo(target, animationSpec)
    }

    /** Scroll to a fraction of the scrollable range. The scrubber's drag lands here. */
    suspend fun scrollToFraction(fraction: Float) {
        val plan = plan ?: return
        scroll { scrollBy(GalleryScroll.scrollForFraction(plan, fraction, viewportHeight) - scrollYState) }
    }

    /** Animate to a fraction of the scrollable range. */
    suspend fun animateScrollToFraction(
        fraction: Float,
        animationSpec: AnimationSpec<Float> = spring(),
    ) {
        val plan = plan ?: return
        animateScrollTo(GalleryScroll.scrollForFraction(plan, fraction, viewportHeight), animationSpec)
    }

    private suspend fun animateScrollTo(
        target: Float,
        animationSpec: AnimationSpec<Float>,
    ) {
        scroll {
            var previous = scrollYState
            animate(initialValue = previous, targetValue = target, animationSpec = animationSpec) { value, _ ->
                previous += scrollBy(value - previous)
            }
        }
    }

    /** Runs [block] with exclusive control of the scroll position. */
    override suspend fun scroll(
        scrollPriority: MutatePriority,
        block: suspend ScrollScope.() -> Unit,
    ) = scrollableState.scroll(scrollPriority, block)

    companion object {
        /**
         * Saves the anchored asset rather than the pixel offset.
         *
         * See the class KDoc: after a rotation the plan is different, so a pixel offset
         * names a different photograph. The anchor names the photograph.
         */
        val Saver: Saver<GalleryState, List<Float>> =
            Saver(
                save = { state ->
                    val anchor = state.captureAnchor()
                    listOf(anchor?.asset?.toFloat() ?: -1f, anchor?.viewportFraction ?: 0f)
                },
                restore = { saved ->
                    val asset = saved.getOrElse(0) { -1f }.toInt()
                    val fraction = saved.getOrElse(1) { 0f }
                    GalleryState(if (asset >= 0) GalleryScroll.Anchor(asset, fraction) else null)
                },
            )
    }
}

/** A [GalleryState] that survives recomposition and configuration change. */
@Composable
fun rememberGalleryState(): GalleryState = rememberSaveable(saver = GalleryState.Saver) { GalleryState() }
