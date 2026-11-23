package com.justin13888.capsule.feature.settings

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.designsystem.theme.LocalReduceMotion
import com.justin13888.capsule.core.designsystem.theme.systemCapsuleContrast
import org.koin.compose.koinInject

/**
 * Puts [content] under the theme the user chose.
 *
 * The appearance settings are only worth having if changing one changes the app, so this is
 * the piece that makes them real: it resolves [CapsuleTheme]'s three parameters from stored
 * preferences instead of from the platform's defaults, and re-composes the subtree the moment
 * one is written.
 *
 * Wrap the highest composable you can. Everything below it is themed; everything above it
 * keeps whatever theme it was already under.
 */
@Composable
fun CapsuleAppearanceHost(content: @Composable () -> Unit) {
    val repository = koinInject<DeviceSettingsRepository>()
    val settings by repository.deviceSettings().collectAsStateWithLifecycle(DeviceSettings())
    CapsuleAppearanceTheme(appearance = settings.appearance, content = content)
}

/**
 * The theme, built from an explicit [appearance] rather than from the platform.
 *
 * Separate from [CapsuleAppearanceHost] so a preview and a test can pin an appearance without
 * a Koin graph, which is the whole reason the theme is worth testing at all: "does dark mode
 * actually apply" is not a question a screenshot of the default answers.
 *
 * Reduce motion is OR'd with whatever the platform already asked for. The app's toggle can
 * add to an accessibility preference set for the whole device; it must never be able to
 * subtract from one.
 */
@Composable
fun CapsuleAppearanceTheme(
    appearance: CapsuleAppearance,
    content: @Composable () -> Unit,
) {
    val darkTheme =
        when (appearance.themeMode) {
            ThemeMode.System -> isSystemInDarkTheme()
            ThemeMode.Light -> false
            ThemeMode.Dark -> true
        }
    CapsuleTheme(
        darkTheme = darkTheme,
        dynamicColor = appearance.dynamicColor,
        contrast = appearance.contrast ?: systemCapsuleContrast(),
    ) {
        CompositionLocalProvider(
            LocalReduceMotion provides resolveReduceMotion(LocalReduceMotion.current, appearance.reduceMotion),
            content = content,
        )
    }
}

/**
 * Whether decorative animation is skipped, given what the platform asks and what the app was
 * told.
 *
 * A plain OR, and a named function so the rule is testable rather than buried in a provider.
 * The asymmetry is the whole point: the app's toggle may *add* to an accessibility preference
 * set for the entire device, and must never be able to switch one back off.
 */
internal fun resolveReduceMotion(
    systemAsks: Boolean,
    appAsks: Boolean,
): Boolean = systemAsks || appAsks
