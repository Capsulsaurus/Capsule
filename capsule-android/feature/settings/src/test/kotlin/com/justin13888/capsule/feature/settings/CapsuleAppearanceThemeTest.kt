package com.justin13888.capsule.feature.settings

import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.test.junit4.v2.createComposeRule
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.designsystem.theme.LocalReduceMotion
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import kotlin.math.abs

/**
 * That the appearance settings actually change the app.
 *
 * The reason this is worth a test rather than an eyeball: a settings screen whose controls
 * persist correctly and change nothing on screen passes every other case in this module. Each
 * case here reads the value the rest of the app reads — the Material colour scheme, and
 * `LocalReduceMotion` — rather than the preference that fed it.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class CapsuleAppearanceThemeTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun choosing_dark_darkens_the_surface_every_screen_sits_on() {
        val (light, dark) =
            schemesUnder(
                CapsuleAppearance(themeMode = ThemeMode.Light),
                CapsuleAppearance(themeMode = ThemeMode.Dark),
            )

        assertNotEquals(light.surface, dark.surface)
        assertTrue(
            "a dark theme's surface must be darker than a light one's",
            dark.surface.luminance() < light.surface.luminance(),
        )
    }

    @Test
    fun raising_the_contrast_changes_the_scheme_rather_than_only_the_preference() {
        val (standard, high) =
            schemesUnder(
                CapsuleAppearance(themeMode = ThemeMode.Light, contrast = CapsuleContrast.Standard),
                CapsuleAppearance(themeMode = ThemeMode.Light, contrast = CapsuleContrast.High),
            )

        // The background tone is held constant across contrast levels by design; what moves is
        // everything drawn *on* it, which is where the separation the user asked for comes from.
        assertNotEquals("raised contrast must move the foreground roles", standard.onSurface, high.onSurface)
        assertNotEquals(standard.outline, high.outline)
        assertTrue(
            "high contrast must widen the gap between text and its background",
            contrastGap(high) > contrastGap(standard),
        )
    }

    @Test
    fun turning_reduce_motion_on_reaches_the_local_every_component_reads() {
        assertEquals(true, reduceMotionUnder(CapsuleAppearance(reduceMotion = true)))
    }

    @Test
    fun leaving_reduce_motion_off_never_overrides_the_system_asking_for_it() {
        // Asserted on the rule rather than through the theme, because the platform's own
        // answer comes from a global setting no composition-local override can stand in for.
        assertTrue(
            "an app toggle may add to an accessibility preference, never subtract from one",
            resolveReduceMotion(systemAsks = true, appAsks = false),
        )
        assertTrue(resolveReduceMotion(systemAsks = false, appAsks = true))
        assertEquals(false, resolveReduceMotion(systemAsks = false, appAsks = false))
    }

    /**
     * The colour scheme each appearance produces, all in one composition.
     *
     * One `setContent` per case is a hard rule of the test rule, so the appearances are
     * composed side by side rather than in sequence — which also proves they are independent
     * rather than leaking through a shared local.
     */
    private fun schemesUnder(vararg appearances: CapsuleAppearance): List<ColorScheme> {
        val schemes = arrayOfNulls<ColorScheme>(appearances.size)
        compose.setContent {
            // Wallpaper colours are off in every case here: the JVM has no wallpaper, and a
            // theme that recoloured itself from the host would make these assertions untestable.
            appearances.forEachIndexed { index, appearance ->
                CapsuleAppearanceTheme(appearance.copy(dynamicColor = false)) {
                    schemes[index] = MaterialTheme.colorScheme
                }
            }
        }
        compose.waitForIdle()
        return schemes.map { requireNotNull(it) { "every appearance must produce a scheme" } }
    }

    /** How far apart body text and the surface behind it sit, in relative luminance. */
    private fun contrastGap(scheme: ColorScheme): Float = abs(scheme.onSurface.luminance() - scheme.surface.luminance())

    private fun reduceMotionUnder(appearance: CapsuleAppearance): Boolean {
        var reduced = false
        compose.setContent {
            CapsuleAppearanceTheme(appearance.copy(dynamicColor = false)) {
                reduced = LocalReduceMotion.current
            }
        }
        compose.waitForIdle()
        return reduced
    }
}
