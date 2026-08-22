package com.justin13888.capsule.core.designsystem

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.designsystem.preview.CapsuleComponentGallery
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Runs Google's Accessibility Test Framework over the component gallery.
 *
 * Contrast ratios and touch-target sizes are claims the design system makes on behalf of
 * every screen in the app, so they are checked mechanically rather than asserted in a
 * comment. This runs on the JVM through Robolectric alongside the screenshot goldens —
 * a failure here means a component is unusable, not merely that it looks different.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class DesignSystemAccessibilityTest {
    @get:Rule
    val compose = createComposeRule()

    private val options =
        RoborazziATFAccessibilityCheckOptions(
            checker = RoborazziATFAccessibilityChecker(),
            failureLevel = RoborazziATFAccessibilityChecker.CheckLevel.Warning,
        )

    private fun checkGallery(
        darkTheme: Boolean,
        contrast: CapsuleContrast,
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = darkTheme, contrast = contrast) {
                Box(Modifier.fillMaxSize()) { CapsuleComponentGallery() }
            }
        }
        compose.onRoot().checkRoboAccessibility(options)
    }

    @Test
    fun gallery_is_accessible_in_dark_theme() {
        checkGallery(darkTheme = true, contrast = CapsuleContrast.Standard)
    }

    @Test
    fun gallery_is_accessible_in_light_theme() {
        checkGallery(darkTheme = false, contrast = CapsuleContrast.Standard)
    }
}
