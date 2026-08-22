package com.justin13888.capsule.core.designsystem

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.captureRoboImage
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
 * Golden screenshots of the whole component gallery.
 *
 * The design system is the one place a visual regression is cheap to catch and expensive
 * to miss — every screen in the app inherits from it — so it is the first thing pinned to
 * goldens. Roborazzi renders through Robolectric on the JVM, so this runs in the ordinary
 * unit-test lane with no emulator; the emulator is reserved for proving the app launches.
 *
 * Regenerate with `mise run screenshot-update-android` after an intentional visual change,
 * and review the resulting image diff like any other part of the change.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class DesignSystemScreenshotTest {
    @get:Rule
    val compose = createComposeRule()

    private fun capture(name: String, darkTheme: Boolean, contrast: CapsuleContrast) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = darkTheme, contrast = contrast) {
                Box(Modifier.fillMaxSize()) { CapsuleComponentGallery() }
            }
        }
        compose.onRoot().captureRoboImage("src/test/screenshots/$name.png")
    }

    @Test
    fun gallery_dark_standard_contrast() {
        capture("gallery-dark-standard", darkTheme = true, contrast = CapsuleContrast.Standard)
    }

    @Test
    fun gallery_light_standard_contrast() {
        capture("gallery-light-standard", darkTheme = false, contrast = CapsuleContrast.Standard)
    }

    @Test
    fun gallery_dark_high_contrast() {
        capture("gallery-dark-high", darkTheme = true, contrast = CapsuleContrast.High)
    }
}
