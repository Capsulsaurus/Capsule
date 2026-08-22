package com.justin13888.capsule.core.designsystem.theme

import android.app.UiModeManager
import android.content.Context
import android.os.Build
import androidx.annotation.RequiresApi
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.platform.LocalContext

/** The platform's contrast reading is continuous; Material defines three steps. */
private const val MEDIUM_CONTRAST_THRESHOLD = 1f / 3f
private const val HIGH_CONTRAST_THRESHOLD = 2f / 3f

/**
 * How far apart the foreground and background colour roles are pushed.
 *
 * A photo library gets looked at in daylight, in a darkroom, and by people who need
 * more separation than the Material baseline gives. Contrast is a first-class theme
 * input rather than an accessibility afterthought, because every role has to move
 * together — raising contrast on a surface but not its outline is how a design system
 * ends up shipping chrome nobody can see.
 */
enum class CapsuleContrast {
    /** The Material baseline. Text pairs clear 4.5:1. */
    Standard,

    /** Text pairs clear 7:1 — the WCAG AAA bar for body copy. */
    Medium,

    /** Text pairs clear 11:1, and so do outlines. For bright sunlight and low vision. */
    High,
}

/**
 * The contrast the platform is asking for, or [CapsuleContrast.Standard] where the
 * platform has no opinion.
 *
 * Android only began reporting a user contrast preference in API 34. Below that the
 * system genuinely cannot tell us, and inferring one from font scale or dark mode would
 * be a guess dressed up as a preference.
 */
@Composable
@ReadOnlyComposable
fun systemCapsuleContrast(): CapsuleContrast = LocalContext.current.systemCapsuleContrast()

/** The non-Compose half of [systemCapsuleContrast], for call sites outside a composition. */
internal fun Context.systemCapsuleContrast(): CapsuleContrast =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
        reportedContrast()
    } else {
        CapsuleContrast.Standard
    }

@RequiresApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
private fun Context.reportedContrast(): CapsuleContrast {
    val uiModeManager = getSystemService(Context.UI_MODE_SERVICE) as? UiModeManager
    return when {
        uiModeManager == null -> CapsuleContrast.Standard
        uiModeManager.contrast >= HIGH_CONTRAST_THRESHOLD -> CapsuleContrast.High
        uiModeManager.contrast >= MEDIUM_CONTRAST_THRESHOLD -> CapsuleContrast.Medium
        else -> CapsuleContrast.Standard
    }
}
