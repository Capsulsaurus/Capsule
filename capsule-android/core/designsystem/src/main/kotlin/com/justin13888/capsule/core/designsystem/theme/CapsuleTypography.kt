package com.justin13888.capsule.core.designsystem.theme

import androidx.compose.material3.Typography
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.Hyphens
import androidx.compose.ui.text.style.LineBreak

/** The Material 3 Expressive scale, before Capsule's line-breaking policy is applied. */
private val Expressive = Typography()

/**
 * Wrapping rules make a bigger difference than a type ramp here.
 *
 * Album and asset titles are user data — filenames, client names, shoot slugs — and they
 * wrap in a grid cell three words wide. [LineBreak.Heading] keeps a title from breaking
 * after a preposition; [LineBreak.Paragraph] plus [Hyphens.Auto] stops body copy in a
 * narrow sheet from opening a river of whitespace. Compose applies neither by default.
 */
private fun TextStyle.asHeading() = copy(lineBreak = LineBreak.Heading)

private fun TextStyle.asParagraph() = copy(lineBreak = LineBreak.Paragraph, hyphens = Hyphens.Auto)

/**
 * The Capsule type scale.
 *
 * It is the Material 3 Expressive scale unchanged in size and weight — the expressive
 * ramp already gives the display sizes the personality this product wants, and a
 * bespoke ramp would be one more thing to keep in sync with the platform. What Capsule
 * adds is a line-breaking policy, applied to each style and its `*Emphasized` twin so
 * that emphasising a heading never changes where it wraps.
 */
val CapsuleTypography =
    Typography(
        displayLarge = Expressive.displayLarge.asHeading(),
        displayMedium = Expressive.displayMedium.asHeading(),
        displaySmall = Expressive.displaySmall.asHeading(),
        headlineLarge = Expressive.headlineLarge.asHeading(),
        headlineMedium = Expressive.headlineMedium.asHeading(),
        headlineSmall = Expressive.headlineSmall.asHeading(),
        titleLarge = Expressive.titleLarge.asHeading(),
        titleMedium = Expressive.titleMedium.asHeading(),
        titleSmall = Expressive.titleSmall.asHeading(),
        bodyLarge = Expressive.bodyLarge.asParagraph(),
        bodyMedium = Expressive.bodyMedium.asParagraph(),
        bodySmall = Expressive.bodySmall.asParagraph(),
        displayLargeEmphasized = Expressive.displayLargeEmphasized.asHeading(),
        displayMediumEmphasized = Expressive.displayMediumEmphasized.asHeading(),
        displaySmallEmphasized = Expressive.displaySmallEmphasized.asHeading(),
        headlineLargeEmphasized = Expressive.headlineLargeEmphasized.asHeading(),
        headlineMediumEmphasized = Expressive.headlineMediumEmphasized.asHeading(),
        headlineSmallEmphasized = Expressive.headlineSmallEmphasized.asHeading(),
        titleLargeEmphasized = Expressive.titleLargeEmphasized.asHeading(),
        titleMediumEmphasized = Expressive.titleMediumEmphasized.asHeading(),
        titleSmallEmphasized = Expressive.titleSmallEmphasized.asHeading(),
        bodyLargeEmphasized = Expressive.bodyLargeEmphasized.asParagraph(),
        bodyMediumEmphasized = Expressive.bodyMediumEmphasized.asParagraph(),
        bodySmallEmphasized = Expressive.bodySmallEmphasized.asParagraph(),
    )

/**
 * Styles Material has no role for.
 *
 * A professional photo library puts a lot of numbers on screen — shutter speeds, ISO,
 * byte counts, chunk offsets — and they are read in columns. Proportional digits make
 * those columns jitter as values change, so the technical readouts get their own style
 * with tabular figures rather than each feature discovering `fontFeatureSettings`.
 */
@Immutable
class CapsuleTextStyles(
    val metadata: TextStyle,
    val metadataEmphasized: TextStyle,
)

/** The technical-readout styles for the current theme. */
val LocalCapsuleTextStyles =
    staticCompositionLocalOf {
        CapsuleTextStyles(
            metadata = CapsuleTypography.bodySmall.copy(fontFeatureSettings = TABULAR_FIGURES),
            metadataEmphasized = CapsuleTypography.labelMedium.copy(fontFeatureSettings = TABULAR_FIGURES),
        )
    }

/** OpenType tabular figures: every digit occupies the same advance width. */
private const val TABULAR_FIGURES = "tnum"
