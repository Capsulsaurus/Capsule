package com.justin13888.capsule.core.designsystem.theme

import android.os.Build
import androidx.annotation.RequiresApi
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.MotionScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext

/**
 * The theme every Capsule surface sits inside.
 *
 * Wraps [MaterialExpressiveTheme] rather than [androidx.compose.material3.MaterialTheme]
 * because the expressive motion scheme is not an opt-in flourish here — the shape morphs,
 * the wavy progress indicators and the button group's squeeze all read as broken under
 * the standard scheme's springs.
 *
 * @param darkTheme follows the system by default. Photographers grade in the dark, so
 *   this is expected to be overridden by an explicit user preference more often than not.
 * @param dynamicColor lets Material You recolour the chrome from the wallpaper on API 31+.
 *   It is on by default because the chrome is nearly achromatic anyway; it is ignored when
 *   [contrast] is raised, since the platform's dynamic schemes have no contrast variants
 *   and silently dropping the user's contrast request would be worse than dropping their
 *   wallpaper colours. The semantic colours in [LocalCapsuleColors] are never dynamic.
 * @param contrast follows the system on API 34+ and is [CapsuleContrast.Standard] below it.
 */
@Composable
fun CapsuleTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = true,
    contrast: CapsuleContrast = systemCapsuleContrast(),
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val colorScheme = resolvedColorScheme(darkTheme, dynamicColor, contrast)
    val semanticColors = remember(darkTheme, contrast) { capsuleColors(darkTheme, contrast) }
    val reduceMotion = remember(context) { context.systemReduceMotion() }

    CompositionLocalProvider(
        LocalCapsuleColors provides semanticColors,
        LocalReduceMotion provides reduceMotion,
    ) {
        MaterialExpressiveTheme(
            colorScheme = colorScheme,
            motionScheme = MotionScheme.expressive(),
            shapes = CapsuleShapes,
            typography = CapsuleTypography,
            content = content,
        )
    }
}

/**
 * Accessors for the parts of the theme Material does not own.
 *
 * Mirrors [androidx.compose.material3.MaterialTheme]'s shape so that a call site reads
 * the same whether it wants a Material role or a Capsule one.
 */
object CapsuleTheme {
    /** The semantic colours for the current theme. */
    val colors: CapsuleColors
        @Composable @ReadOnlyComposable
        get() = LocalCapsuleColors.current

    /** The technical-readout text styles for the current theme. */
    val textStyles: CapsuleTextStyles
        @Composable @ReadOnlyComposable
        get() = LocalCapsuleTextStyles.current

    /** Whether decorative animation should be skipped. */
    val reduceMotion: Boolean
        @Composable @ReadOnlyComposable
        get() = LocalReduceMotion.current
}

@Composable
private fun resolvedColorScheme(
    darkTheme: Boolean,
    dynamicColor: Boolean,
    contrast: CapsuleContrast,
): ColorScheme {
    // Material You is only consulted at standard contrast: the platform's dynamic
    // schemes come in one contrast level, so honouring a raised contrast request means
    // falling back to the brand ramps, where the tone table guarantees the separation.
    val wallpaperColors = dynamicColor && contrast == CapsuleContrast.Standard
    return if (wallpaperColors && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        dynamicColorScheme(darkTheme)
    } else {
        capsuleColorScheme(darkTheme, contrast)
    }
}

@RequiresApi(Build.VERSION_CODES.S)
@Composable
private fun dynamicColorScheme(darkTheme: Boolean): ColorScheme {
    val context = LocalContext.current
    return if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
}
