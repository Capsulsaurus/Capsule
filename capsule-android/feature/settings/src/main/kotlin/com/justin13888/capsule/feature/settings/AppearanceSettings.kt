package com.justin13888.capsule.feature.settings

import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleDivider
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.i18n.R

/** Test handle for the per-app-language-unavailable banner. */
const val SETTINGS_LANGUAGE_UNSUPPORTED_TAG: String = "settings.language.unsupported"

/**
 * Theme, colour, contrast, grid density, motion — and the app's own language.
 *
 * Language lives here rather than in a page of its own because there is no destination for it
 * and, more to the point, because "how the app looks" is where somebody goes to find it. The
 * settings on this page are the ones that change what the user sees the instant they are
 * written, which is why every one of them is applied through the theme rather than stored and
 * honoured later.
 */
@Composable
internal fun ColumnScope.AppearanceSettings(
    state: SettingsUiState,
    actions: SettingsActions,
) {
    val appearance = state.device.appearance
    SettingsChoiceGroup(
        group = THEME_GROUP,
        title = stringResource(R.string.android_settings_appearance_theme),
        summary = stringResource(R.string.android_settings_appearance_theme_summary),
        options = themeOptions(),
        selected = appearance.themeMode,
        onSelect = actions.onThemeMode,
    )
    // Material You is only consulted at standard contrast, because the platform's dynamic
    // schemes come in one contrast level. Saying so on the row is better than letting the
    // toggle appear to do nothing.
    val dynamicColorAvailable = appearance.contrast == null || appearance.contrast == CapsuleContrast.Standard
    SettingsToggleRow(
        id = DYNAMIC_COLOR_TOGGLE,
        title = stringResource(R.string.android_settings_appearance_dynamic_color),
        summary =
            if (dynamicColorAvailable) {
                stringResource(R.string.android_settings_appearance_dynamic_color_summary)
            } else {
                stringResource(R.string.android_settings_appearance_dynamic_color_unavailable)
            },
        checked = appearance.dynamicColor,
        onCheckedChange = actions.onDynamicColor,
        enabled = dynamicColorAvailable,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsChoiceGroup(
        group = CONTRAST_GROUP,
        title = stringResource(R.string.android_settings_appearance_contrast),
        summary = stringResource(R.string.android_settings_appearance_contrast_summary),
        options = contrastOptions(),
        selected = appearance.contrast,
        onSelect = actions.onContrast,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsChoiceGroup(
        group = DENSITY_GROUP,
        title = stringResource(R.string.android_settings_appearance_density),
        summary = stringResource(R.string.android_settings_appearance_density_summary),
        options = densityOptions(),
        selected = appearance.gridAssetsAcross,
        onSelect = actions.onGridAssetsAcross,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsToggleRow(
        id = REDUCE_MOTION_TOGGLE,
        title = stringResource(R.string.android_settings_appearance_reduce_motion),
        summary = stringResource(R.string.android_settings_appearance_reduce_motion_summary),
        checked = appearance.reduceMotion,
        onCheckedChange = actions.onReduceMotion,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    LanguageSettings(state, actions)
}

@Composable
private fun ColumnScope.LanguageSettings(
    state: SettingsUiState,
    actions: SettingsActions,
) {
    CapsuleSectionHeader(title = stringResource(R.string.android_settings_language_title))
    if (!state.perAppLanguageSupported) {
        CapsuleBanner(
            title = stringResource(R.string.android_settings_language_unsupported_title),
            modifier =
                Modifier
                    .fillMaxWidth()
                    .padding(SettingsInset)
                    .testTag(SETTINGS_LANGUAGE_UNSUPPORTED_TAG),
            body = stringResource(R.string.android_settings_language_unsupported_body),
            severity = CapsuleBannerSeverity.Info,
        )
    }
    SettingsChoiceGroup(
        group = LANGUAGE_GROUP,
        title = stringResource(R.string.android_settings_language_title),
        summary = stringResource(R.string.android_settings_language_summary),
        options = languageOptions(),
        selected = state.device.languageTag,
        onSelect = actions.onLanguage,
    )
    SettingsValueRow(
        label = stringResource(R.string.android_settings_language_region),
        value = currentRegionLabel(),
    )
}

@Composable
private fun themeOptions(): List<SettingsChoice<ThemeMode>> =
    listOf(
        SettingsChoice(ThemeMode.System, stringResource(R.string.mode_system), "system"),
        SettingsChoice(ThemeMode.Light, stringResource(R.string.mode_light), "light"),
        SettingsChoice(ThemeMode.Dark, stringResource(R.string.mode_dark), "dark"),
    )

@Composable
private fun contrastOptions(): List<SettingsChoice<CapsuleContrast?>> =
    listOf(
        SettingsChoice(null, stringResource(R.string.android_settings_language_system), "system"),
        SettingsChoice(
            CapsuleContrast.Standard,
            stringResource(R.string.android_settings_appearance_contrast_standard),
            "standard",
        ),
        SettingsChoice(
            CapsuleContrast.Medium,
            stringResource(R.string.android_settings_appearance_contrast_medium),
            "medium",
        ),
        SettingsChoice(
            CapsuleContrast.High,
            stringResource(R.string.android_settings_appearance_contrast_high),
            "high",
        ),
    )

@Composable
private fun densityOptions(): List<SettingsChoice<Int>> =
    CapsuleAppearance.GRID_CHOICES.map { across ->
        SettingsChoice(across, gridDensityLabel(across), across.toString())
    }

@Composable
private fun languageOptions(): List<SettingsChoice<String?>> =
    listOf(SettingsChoice<String?>(null, stringResource(R.string.android_settings_language_system), "system")) +
        CapsuleLanguage.ALL.map { language ->
            SettingsChoice<String?>(language.tag, language.endonym, language.tag)
        }

private const val THEME_GROUP = "theme"
private const val CONTRAST_GROUP = "contrast"
private const val DENSITY_GROUP = "density"
private const val LANGUAGE_GROUP = "language"

/** Test handle id for the wallpaper-colour toggle. */
internal const val DYNAMIC_COLOR_TOGGLE = "dynamic-color"

/** Test handle id for the reduce-motion toggle. */
internal const val REDUCE_MOTION_TOGGLE = "reduce-motion"
