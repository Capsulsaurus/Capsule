package com.justin13888.capsule.core.ui.gallery

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme

/**
 * The fixture the Compose tests render against.
 *
 * Every test in this package pins the Robolectric qualifiers to `mdpi`, so one density
 * pixel is one dp and the plan can be solved in the same numbers the layout is asserted in.
 * Without that the tests would be asserting against a density they never chose.
 */
internal object GalleryFixture {
    /** The viewport the harness renders into, in pixels — and, at mdpi, in dp. */
    const val VIEWPORT_WIDTH = 400f
    const val VIEWPORT_HEIGHT = 800f

    /** Ordinary browsing metrics: roughly three across, with a day header. */
    val Metrics =
        GalleryMetrics(
            viewportWidth = VIEWPORT_WIDTH,
            targetRowHeight = 120f,
            spacing = 4f,
            headerHeight = 44f,
            sectionSpacing = 12f,
        )

    /**
     * A plan of [assets] photographs cut into sections of [perSection].
     *
     * Aspect ratios alternate landscape and portrait so the row solver produces rows of
     * differing lengths — a fixture of identical squares would hide every off-by-one the
     * placement code could have.
     */
    fun plan(
        assets: Int = 600,
        perSection: Int = 20,
        metrics: GalleryMetrics = Metrics,
    ): LayoutPlan {
        val aspects = FloatArray(assets) { if (it % 3 == 0) 0.75f else 1.5f }
        val starts = IntArray((assets + perSection - 1) / perSection) { it * perSection }
        return LayoutPlanner.plan(aspects, starts, metrics)
    }

    /** The test tag a header carries, so a test can tell the pinned copy from the scrolling one. */
    fun headerTag(
        section: Int,
        pinned: Boolean,
    ): String = if (pinned) "pinned-$section" else "header-$section"
}

/**
 * A [JustifiedGallery] in a viewport of known size, with instrumented content slots.
 *
 * @param onCellComposed called for every asset the gallery actually composes. Counting
 *   through the content slot rather than through the semantics tree is what lets a test
 *   distinguish "not visible" from "composed but off screen".
 */
@Composable
internal fun GalleryHarness(
    plan: LayoutPlan,
    state: GalleryState,
    selection: SelectionState? = null,
    zoom: GalleryZoomState? = null,
    onCellComposed: (Int) -> Unit = {},
) {
    CapsuleTheme {
        Box(modifier = Modifier.size(GalleryFixture.VIEWPORT_WIDTH.dp, GalleryFixture.VIEWPORT_HEIGHT.dp)) {
            JustifiedGallery(
                plan = plan,
                state = state,
                selection = selection,
                zoom = zoom,
                cellDescription = { "asset-$it" },
                header = { info ->
                    Box(
                        modifier =
                            Modifier
                                .fillMaxSize()
                                .testTag(GalleryFixture.headerTag(info.section, info.isPinned)),
                    )
                },
                cell = { asset, _, _ ->
                    onCellComposed(asset)
                    Box(Modifier.fillMaxSize().testTag("cell-$asset"))
                },
            )
        }
    }
}
