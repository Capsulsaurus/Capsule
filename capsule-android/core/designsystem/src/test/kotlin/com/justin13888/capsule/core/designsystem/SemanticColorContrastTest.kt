package com.justin13888.capsule.core.designsystem

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.test.junit4.v2.createComposeRule
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.designsystem.theme.CapsuleSemanticColor
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import kotlin.math.max
import kotlin.math.min

/**
 * The semantic colours have to be legible wherever the app puts them.
 *
 * They are the ones Material has no slot for — selection, sync state, quarantine — so
 * nothing upstream is checking them, and they are exactly the colours a feature reaches
 * for when it needs to say something urgent. A sync marker nobody can see is a sync
 * marker that does not exist.
 *
 * Two floors, because the two roles are used differently. The accent is what a feature
 * draws a mark or an icon with directly on the surface, so it takes WCAG's 3:1 for a
 * graphical object. `onContainer` is prose sitting on its own container, so it takes the
 * 4.5:1 for text.
 *
 * `container` is deliberately *not* checked against surface. A Material container is a
 * subtle tint of the ground it sits on — measuring it at ~1.2:1 here is the palette
 * working as intended, not a defect, and asserting otherwise would have forced the
 * containers loud enough to compete with the photographs.
 */
@RunWith(RobolectricTestRunner::class)
class SemanticColorContrastTest {
    @get:Rule
    val compose = createComposeRule()

    private fun contrast(
        a: Color,
        b: Color,
    ): Float {
        val la = a.luminance() + 0.05f
        val lb = b.luminance() + 0.05f
        return max(la, lb) / min(la, lb)
    }

    @Composable
    private fun named(): List<Pair<String, CapsuleSemanticColor>> {
        val colors = com.justin13888.capsule.core.designsystem.theme.CapsuleTheme.colors
        return listOf(
            "selected" to colors.selected,
            "syncPending" to colors.syncPending,
            "syncFailed" to colors.syncFailed,
            "quarantine" to colors.quarantine,
            "degraded" to colors.degraded,
            "success" to colors.success,
        )
    }

    @Test
    fun every_semantic_colour_is_legible_in_every_theme() {
        val failures = mutableListOf<String>()
        // Every theme is rendered inside one composition: the rule allows a single
        // setContent, and each CapsuleTheme supplies its own scheme to what it wraps.
        compose.setContent {
            for (dark in listOf(false, true)) {
                for (level in CapsuleContrast.entries) {
                    CapsuleTheme(darkTheme = dark, dynamicColor = false, contrast = level) {
                        val surface = MaterialTheme.colorScheme.surface
                        named().forEach { (name, semantic) ->
                            val where = "$name ${if (dark) "dark" else "light"}/$level"
                            val accentOnSurface = contrast(semantic.color, surface)
                            val textOnContainer = contrast(semantic.onContainer, semantic.container)
                            if (accentOnSurface < MINIMUM_GRAPHICAL) {
                                failures += "$where: accent vs surface is %.2f:1".format(accentOnSurface)
                            }
                            if (textOnContainer < MINIMUM_TEXT) {
                                failures += "$where: onContainer vs container is %.2f:1".format(textOnContainer)
                            }
                        }
                    }
                }
            }
        }
        compose.waitForIdle()
        assertTrue(failures.joinToString("\n"), failures.isEmpty())
    }

    private companion object {
        /**
         * WCAG 1.4.11: a graphical object or UI component needs 3:1. This is the floor for
         * the accent itself, which is what a feature draws a mark or an icon with.
         */
        const val MINIMUM_GRAPHICAL = 3.0f

        /** WCAG 1.4.3: text needs 4.5:1 against what it sits on. */
        const val MINIMUM_TEXT = 4.5f
    }
}
