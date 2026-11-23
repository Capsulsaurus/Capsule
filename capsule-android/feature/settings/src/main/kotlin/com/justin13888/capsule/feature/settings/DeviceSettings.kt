package com.justin13888.capsule.feature.settings

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.ui.gallery.GalleryDensityLevels

/** Which colour scheme the app uses, and whether it decides for itself. */
enum class ThemeMode {
    /** Follow the device's light/dark setting. */
    System,

    /** Always light. */
    Light,

    /** Always dark. */
    Dark,
}

/**
 * How the app looks, as one value the theme can be built from.
 *
 * Held here rather than in `core:model` because none of it crosses the wire or belongs to the
 * library: two devices signed into the same account are entitled to different themes, and a
 * contrast level synced from a tablet would be a worse default than the phone's own.
 *
 * [contrast] is nullable rather than carrying a fourth "system" constant, because the platform
 * genuinely reports a contrast preference and "no override" is exactly the absence of a
 * choice, not a fourth choice.
 */
@Immutable
data class CapsuleAppearance(
    /** Light, dark, or whatever the device says. */
    val themeMode: ThemeMode = ThemeMode.System,
    /** Whether Material You may recolour the chrome from the wallpaper. */
    val dynamicColor: Boolean = true,
    /** The contrast override, or `null` to follow the platform's own reading. */
    val contrast: CapsuleContrast? = null,
    /** How many photos the library grid opens at across a row. */
    val gridAssetsAcross: Int = DEFAULT_GRID_ACROSS,
    /**
     * Whether decorative animation is skipped.
     *
     * One-way on purpose: it can only ever *add* to the system-wide accessibility setting. An
     * app toggle able to switch that setting back off would override an accessibility
     * preference the user set for the whole device.
     */
    val reduceMotion: Boolean = false,
) {
    companion object {
        /** The rung the library opens at when the user has expressed no preference. */
        val DEFAULT_GRID_ACROSS: Int =
            GalleryDensityLevels.Compact.levels[GalleryDensityLevels.Compact.defaultIndex].assetsAcross

        /** The rungs a phone offers, coarsest first so the list reads from big to small. */
        val GRID_CHOICES: List<Int> =
            GalleryDensityLevels.Compact.levels
                .map { it.assetsAcross }
                .sorted()
    }
}

/**
 * A language Capsule ships text in.
 *
 * The endonym is deliberately not a catalogue key: a language list that renders every entry in
 * the *current* language is unusable to the one person who most needs it — someone who cannot
 * read the current language and is looking for their own.
 */
@Immutable
data class CapsuleLanguage(
    /** The BCP 47 tag, as the per-app locale API expects it. */
    val tag: String,
    /** The language's name in itself. */
    val endonym: String,
) {
    companion object {
        /** Every language the catalogue is generated into, alphabetical by tag. */
        val ALL: List<CapsuleLanguage> =
            listOf(
                CapsuleLanguage("ar", "العربية"),
                CapsuleLanguage("de", "Deutsch"),
                CapsuleLanguage("en", "English"),
                CapsuleLanguage("es", "Español"),
                CapsuleLanguage("fr", "Français"),
                CapsuleLanguage("hi", "हिन्दी"),
                CapsuleLanguage("it", "Italiano"),
                CapsuleLanguage("ja", "日本語"),
                CapsuleLanguage("ko", "한국어"),
                CapsuleLanguage("pt-BR", "Português (Brasil)"),
                CapsuleLanguage("ru", "Русский"),
                CapsuleLanguage("zh-Hans", "简体中文"),
                CapsuleLanguage("zh-Hant", "繁體中文"),
            )
    }
}

/**
 * Every preference that belongs to this device rather than to the library.
 *
 * The split from `LibrarySettings` is not organisational. Those settings change what happens
 * to the user's bytes and are therefore the library's, synced and shared; these change what
 * this screen looks like and what this build reports about itself, and syncing them would
 * mean a tablet's contrast preference arriving on a phone as a surprise.
 */
@Immutable
data class DeviceSettings(
    /** How the app looks. */
    val appearance: CapsuleAppearance = CapsuleAppearance(),
    /** The chosen app language tag, or `null` to follow the device. */
    val languageTag: String? = null,
    /**
     * Whether crash and performance reports may be sent.
     *
     * Off until asked. Consent that defaults to granted is not consent, and this is the one
     * setting on this screen that sends anything anywhere without the user acting first.
     */
    val diagnosticsConsent: Boolean = false,
)
