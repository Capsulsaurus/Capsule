package com.justin13888.capsule.core.ui.gallery

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.captureRoboImage
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Goldens of the grid as it is actually drawn.
 *
 * The row solver has thorough numeric tests and the Compose layer has thorough semantic
 * ones, and between them they still could not see that the pinned header was painting over
 * the entire library: every cell measured, placed and announced correctly underneath an
 * opaque band. Bounds and semantics describe a layout; only pixels describe a picture, so
 * these exist to be *looked at*.
 *
 * Cells are flat colour rather than the mock's procedural imagery — this module deliberately
 * does not depend on `core:mock`, and a solid fill makes the row geometry far easier to read
 * in a diff than a photograph would.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w400dp-h800dp-mdpi")
class GalleryScreenshotTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun grid_at_browsing_density() {
        capture(name = "gallery-browsing", targetRowHeight = 120f)
    }

    @Test
    fun grid_at_a_coarser_density() {
        capture(name = "gallery-coarse", targetRowHeight = 200f)
    }

    @Test
    fun grid_scrolled_into_a_later_section() {
        // The state the regression hid in: a header pinned over live content rather than
        // sitting at the top of the plan where it happens to belong anyway.
        capture(name = "gallery-pinned", targetRowHeight = 120f, scrollBy = 1_400f)
    }

    private fun capture(
        name: String,
        targetRowHeight: Float,
        scrollBy: Float = 0f,
    ) {
        val metrics = GalleryFixture.Metrics.copy(targetRowHeight = targetRowHeight)
        val plan = GalleryFixture.plan(assets = 240, perSection = 17, metrics = metrics)
        val state = GalleryState()
        compose.setContent {
            CapsuleTheme(darkTheme = true, dynamicColor = false) {
                Box(
                    Modifier
                        .size(GalleryFixture.VIEWPORT_WIDTH.dp, GalleryFixture.VIEWPORT_HEIGHT.dp)
                        .background(MaterialTheme.colorScheme.surface),
                ) {
                    JustifiedGallery(
                        plan = plan,
                        state = state,
                        header = { info ->
                            // A tinted band rather than a date label: this golden is about
                            // geometry, and real copy would make every diff a text diff. The
                            // pinned copy is deliberately the brighter of the two so its
                            // extent is obvious — it is what regressed.
                            Box(
                                Modifier
                                    .fillMaxSize()
                                    .background(
                                        if (info.isPinned) {
                                            MaterialTheme.colorScheme.surfaceContainerHighest
                                        } else {
                                            MaterialTheme.colorScheme.surfaceContainer
                                        },
                                    ),
                            )
                        },
                        cell = { asset, _, _ ->
                            // The gallery has already sized the slot from the plan; the cell
                            // fills it, exactly as a real thumbnail would.
                            Box(Modifier.fillMaxSize().background(hueFor(asset)))
                        },
                    )
                }
            }
        }
        compose.waitForIdle()
        if (scrollBy != 0f) {
            compose.runOnIdle { state.dispatchRawDelta(scrollBy) }
            compose.waitForIdle()
        }
        compose.onRoot().captureRoboImage("src/test/screenshots/$name.png")
    }

    /**
     * A stable, varied hue per asset.
     *
     * Adjacent assets must not share a colour or the golden cannot show where one cell ends
     * and the next begins — which is the single thing a justified grid has to get right.
     */
    private fun hueFor(asset: Int): Color {
        val hue = (asset * HUE_STEP) % HUE_WHEEL
        val sector = hue / HUE_SECTOR
        val fraction = (hue % HUE_SECTOR) / HUE_SECTOR.toFloat()
        val rising = fraction
        val falling = 1f - fraction
        return when (sector) {
            0 -> Color(0.85f, rising * 0.7f + 0.15f, 0.2f)
            1 -> Color(falling * 0.7f + 0.15f, 0.8f, 0.25f)
            2 -> Color(0.2f, 0.8f, rising * 0.7f + 0.2f)
            3 -> Color(0.2f, falling * 0.6f + 0.25f, 0.85f)
            4 -> Color(rising * 0.6f + 0.3f, 0.25f, 0.85f)
            else -> Color(0.85f, 0.25f, falling * 0.6f + 0.25f)
        }
    }

    private companion object {
        /** Coprime with the wheel, so no two nearby assets land on the same hue. */
        const val HUE_STEP = 47
        const val HUE_WHEEL = 360
        const val HUE_SECTOR = 60
    }
}
