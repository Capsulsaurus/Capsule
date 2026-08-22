package com.justin13888.capsule.core.designsystem.theme

import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color

/**
 * One semantic meaning, in the four roles Material would give an accent.
 *
 * [color] is for the mark itself (a dot, a stroke, an icon); [container] is for a filled
 * chip or banner behind text. Both carry their own on-colour so a caller can never pair a
 * container with the wrong foreground.
 */
@Immutable
class CapsuleSemanticColor(
    val color: Color,
    val onColor: Color,
    val container: Color,
    val onContainer: Color,
)

/**
 * The colours the product needs that Material has no slot for.
 *
 * Material's roles describe emphasis, not meaning. A photo library has states that are
 * neither "error" nor "primary" — a blob the server has not acknowledged, an asset held
 * by moderation, a library running on proxies because the originals are cold. Encoding
 * those as first-class colours stops each feature from inventing its own amber.
 *
 * These deliberately do not follow Material You: they are the only colours in the app
 * whose meaning is fixed, and a wallpaper must not be allowed to make "sync failed" and
 * "success" the same hue.
 */
@Immutable
class CapsuleColors(
    val selected: CapsuleSemanticColor,
    val syncPending: CapsuleSemanticColor,
    val syncFailed: CapsuleSemanticColor,
    val quarantine: CapsuleSemanticColor,
    val degraded: CapsuleSemanticColor,
    val success: CapsuleSemanticColor,
)

/**
 * The semantic colours for the current theme.
 *
 * The default is the light standard-contrast set rather than an error: a component
 * rendered outside [CapsuleTheme] — an isolated preview, a screenshot harness — should
 * still draw something legible instead of taking the process down.
 */
val LocalCapsuleColors =
    staticCompositionLocalOf {
        capsuleColors(darkTheme = false, contrast = CapsuleContrast.Standard)
    }

/** Builds the semantic set for one light/dark and contrast combination. */
internal fun capsuleColors(
    darkTheme: Boolean,
    contrast: CapsuleContrast,
): CapsuleColors {
    val tones = capsuleToneSpec(darkTheme, contrast).accent
    return CapsuleColors(
        selected = PrimaryRamp.semantic(tones),
        syncPending = SyncPendingRamp.semantic(tones),
        syncFailed = SyncFailedRamp.semantic(tones),
        quarantine = QuarantineRamp.semantic(tones),
        degraded = DegradedRamp.semantic(tones),
        success = SuccessRamp.semantic(tones),
    )
}

private fun CapsuleRamp.semantic(tones: AccentTones) =
    CapsuleSemanticColor(
        color = tone(tones.accent),
        onColor = tone(tones.onAccent),
        container = tone(tones.container),
        onContainer = tone(tones.onContainer),
    )
