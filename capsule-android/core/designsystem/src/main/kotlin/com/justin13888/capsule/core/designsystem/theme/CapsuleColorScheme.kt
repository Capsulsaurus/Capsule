package com.justin13888.capsule.core.designsystem.theme

import androidx.compose.material3.ColorScheme

/** The four tones an accent role needs: the accent, its container, and both on-colours. */
internal class AccentTones(
    val accent: Int,
    val onAccent: Int,
    val container: Int,
    val onContainer: Int,
)

/** Surface and its variant, plus the two extremes a surface can be dimmed or brightened to. */
internal class SurfaceTones(
    val surface: Int,
    val onSurface: Int,
    val variant: Int,
    val onVariant: Int,
    val dim: Int,
    val bright: Int,
)

/** The five stacked container levels, lowest (furthest back) to highest (nearest). */
internal class ContainerTones(
    val lowest: Int,
    val low: Int,
    val base: Int,
    val high: Int,
    val highest: Int,
)

/** The inverted island — snackbars and the tooltip surface. */
internal class InverseTones(
    val surface: Int,
    val onSurface: Int,
    val accent: Int,
)

/** Roles that deliberately do not flip between light and dark. */
internal class FixedTones(
    val fixed: Int,
    val fixedDim: Int,
    val onFixed: Int,
    val onFixedVariant: Int,
)

/**
 * Every Material role expressed as a tone rather than a colour.
 *
 * Separating "which tone" from "which hue" is what lets one small table drive six colour
 * schemes across eleven ramps. It also makes the contrast argument checkable by reading:
 * the tone deltas here are the guarantee, and they hold for every ramp because the ramps
 * are generated at a fixed lightness curve.
 */
internal class CapsuleToneSpec(
    val accent: AccentTones,
    val surface: SurfaceTones,
    val container: ContainerTones,
    val inverse: InverseTones,
    val fixed: FixedTones,
    val outline: Int,
    val outlineVariant: Int,
)

private val LightStandard =
    CapsuleToneSpec(
        accent = AccentTones(accent = 40, onAccent = 100, container = 90, onContainer = 30),
        surface = SurfaceTones(surface = 98, onSurface = 10, variant = 90, onVariant = 30, dim = 87, bright = 98),
        container = ContainerTones(lowest = 100, low = 96, base = 94, high = 92, highest = 90),
        inverse = InverseTones(surface = 20, onSurface = 95, accent = 80),
        fixed = FixedTones(fixed = 90, fixedDim = 80, onFixed = 10, onFixedVariant = 30),
        outline = 50,
        outlineVariant = 80,
    )

private val LightMedium =
    CapsuleToneSpec(
        accent = AccentTones(accent = 30, onAccent = 100, container = 90, onContainer = 12),
        surface = SurfaceTones(surface = 98, onSurface = 6, variant = 90, onVariant = 20, dim = 87, bright = 98),
        container = ContainerTones(lowest = 100, low = 96, base = 94, high = 92, highest = 90),
        inverse = InverseTones(surface = 20, onSurface = 96, accent = 87),
        fixed = FixedTones(fixed = 90, fixedDim = 80, onFixed = 6, onFixedVariant = 20),
        outline = 40,
        outlineVariant = 60,
    )

private val LightHigh =
    CapsuleToneSpec(
        accent = AccentTones(accent = 20, onAccent = 100, container = 90, onContainer = 0),
        surface = SurfaceTones(surface = 98, onSurface = 0, variant = 90, onVariant = 10, dim = 87, bright = 98),
        container = ContainerTones(lowest = 100, low = 96, base = 94, high = 92, highest = 90),
        inverse = InverseTones(surface = 20, onSurface = 100, accent = 92),
        fixed = FixedTones(fixed = 90, fixedDim = 80, onFixed = 0, onFixedVariant = 10),
        outline = 22,
        outlineVariant = 40,
    )

private val DarkStandard =
    CapsuleToneSpec(
        accent = AccentTones(accent = 80, onAccent = 20, container = 30, onContainer = 90),
        surface = SurfaceTones(surface = 6, onSurface = 90, variant = 30, onVariant = 80, dim = 6, bright = 24),
        container = ContainerTones(lowest = 4, low = 10, base = 12, high = 17, highest = 22),
        inverse = InverseTones(surface = 90, onSurface = 20, accent = 40),
        fixed = FixedTones(fixed = 90, fixedDim = 80, onFixed = 10, onFixedVariant = 30),
        outline = 60,
        outlineVariant = 30,
    )

private val DarkMedium =
    CapsuleToneSpec(
        accent = AccentTones(accent = 87, onAccent = 17, container = 30, onContainer = 95),
        surface = SurfaceTones(surface = 6, onSurface = 96, variant = 24, onVariant = 92, dim = 6, bright = 24),
        container = ContainerTones(lowest = 4, low = 10, base = 12, high = 17, highest = 22),
        inverse = InverseTones(surface = 92, onSurface = 12, accent = 30),
        fixed = FixedTones(fixed = 92, fixedDim = 80, onFixed = 6, onFixedVariant = 20),
        outline = 70,
        outlineVariant = 50,
    )

private val DarkHigh =
    CapsuleToneSpec(
        accent = AccentTones(accent = 95, onAccent = 6, container = 24, onContainer = 100),
        surface = SurfaceTones(surface = 6, onSurface = 100, variant = 22, onVariant = 98, dim = 6, bright = 24),
        container = ContainerTones(lowest = 4, low = 10, base = 12, high = 17, highest = 22),
        inverse = InverseTones(surface = 95, onSurface = 6, accent = 24),
        fixed = FixedTones(fixed = 95, fixedDim = 87, onFixed = 0, onFixedVariant = 10),
        outline = 92,
        outlineVariant = 70,
    )

/** The tone table for one light/dark and contrast combination. */
internal fun capsuleToneSpec(
    darkTheme: Boolean,
    contrast: CapsuleContrast,
): CapsuleToneSpec =
    if (darkTheme) {
        when (contrast) {
            CapsuleContrast.Standard -> DarkStandard
            CapsuleContrast.Medium -> DarkMedium
            CapsuleContrast.High -> DarkHigh
        }
    } else {
        when (contrast) {
            CapsuleContrast.Standard -> LightStandard
            CapsuleContrast.Medium -> LightMedium
            CapsuleContrast.High -> LightHigh
        }
    }

/**
 * The Capsule brand colour scheme for one light/dark and contrast combination.
 *
 * Built from the tone table rather than a list of hex values so that adding a contrast
 * level or retuning a hue is a one-line change that cannot desynchronise the roles.
 */
@Suppress("LongMethod")
internal fun capsuleColorScheme(
    darkTheme: Boolean,
    contrast: CapsuleContrast,
): ColorScheme {
    val spec = capsuleToneSpec(darkTheme, contrast)
    val accent = spec.accent
    val surface = spec.surface
    val container = spec.container
    val inverse = spec.inverse
    val fixed = spec.fixed
    return ColorScheme(
        primary = PrimaryRamp.tone(accent.accent),
        onPrimary = PrimaryRamp.tone(accent.onAccent),
        primaryContainer = PrimaryRamp.tone(accent.container),
        onPrimaryContainer = PrimaryRamp.tone(accent.onContainer),
        inversePrimary = PrimaryRamp.tone(inverse.accent),
        secondary = SecondaryRamp.tone(accent.accent),
        onSecondary = SecondaryRamp.tone(accent.onAccent),
        secondaryContainer = SecondaryRamp.tone(accent.container),
        onSecondaryContainer = SecondaryRamp.tone(accent.onContainer),
        tertiary = TertiaryRamp.tone(accent.accent),
        onTertiary = TertiaryRamp.tone(accent.onAccent),
        tertiaryContainer = TertiaryRamp.tone(accent.container),
        onTertiaryContainer = TertiaryRamp.tone(accent.onContainer),
        background = NeutralRamp.tone(surface.surface),
        onBackground = NeutralRamp.tone(surface.onSurface),
        surface = NeutralRamp.tone(surface.surface),
        onSurface = NeutralRamp.tone(surface.onSurface),
        surfaceVariant = NeutralVariantRamp.tone(surface.variant),
        onSurfaceVariant = NeutralVariantRamp.tone(surface.onVariant),
        surfaceTint = PrimaryRamp.tone(accent.accent),
        inverseSurface = NeutralRamp.tone(inverse.surface),
        inverseOnSurface = NeutralRamp.tone(inverse.onSurface),
        error = ErrorRamp.tone(accent.accent),
        onError = ErrorRamp.tone(accent.onAccent),
        errorContainer = ErrorRamp.tone(accent.container),
        onErrorContainer = ErrorRamp.tone(accent.onContainer),
        outline = NeutralVariantRamp.tone(spec.outline),
        outlineVariant = NeutralVariantRamp.tone(spec.outlineVariant),
        scrim = NeutralRamp.tone(0),
        surfaceBright = NeutralRamp.tone(surface.bright),
        surfaceDim = NeutralRamp.tone(surface.dim),
        surfaceContainer = NeutralRamp.tone(container.base),
        surfaceContainerHigh = NeutralRamp.tone(container.high),
        surfaceContainerHighest = NeutralRamp.tone(container.highest),
        surfaceContainerLow = NeutralRamp.tone(container.low),
        surfaceContainerLowest = NeutralRamp.tone(container.lowest),
        primaryFixed = PrimaryRamp.tone(fixed.fixed),
        primaryFixedDim = PrimaryRamp.tone(fixed.fixedDim),
        onPrimaryFixed = PrimaryRamp.tone(fixed.onFixed),
        onPrimaryFixedVariant = PrimaryRamp.tone(fixed.onFixedVariant),
        secondaryFixed = SecondaryRamp.tone(fixed.fixed),
        secondaryFixedDim = SecondaryRamp.tone(fixed.fixedDim),
        onSecondaryFixed = SecondaryRamp.tone(fixed.onFixed),
        onSecondaryFixedVariant = SecondaryRamp.tone(fixed.onFixedVariant),
        tertiaryFixed = TertiaryRamp.tone(fixed.fixed),
        tertiaryFixedDim = TertiaryRamp.tone(fixed.fixedDim),
        onTertiaryFixed = TertiaryRamp.tone(fixed.onFixed),
        onTertiaryFixedVariant = TertiaryRamp.tone(fixed.onFixedVariant),
    )
}
