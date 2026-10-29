package com.justin13888.capsule.i18n

import java.util.Locale

/**
 * The locale every formatter is given, so that the system's region decides how dates, times
 * and numbers are written.
 *
 * Language and region are two settings. The language is the per-app language the user
 * picked in system settings (Android 13+, declared through `localeConfig`) or in the app's
 * own picker ([AppLanguagePicker], below Android 13), or the system language when they
 * picked none; it decides which strings are shown. The region is always
 * the system's, and it decides how dates, times and numbers are written.
 *
 * Where the platform has formatting data for the app language in the system's region, the
 * result is that pair: English on a phone set to German yields `en-DE`, English month
 * names with German grouping, and Hong Kong Chinese on a phone set to Taiwan yields
 * `zh-Hant-TW`. Where it has none (CLDR has no
 * `zh-Hant-GB`, `fr-US` or `ja-DE`), the pair would format exactly as the bare language and
 * the region would have no effect, so the system locale itself is used: Hong Kong Chinese
 * on a phone set to British English formats as `en-GB`, British day-month order with
 * English month names.
 *
 * The system locale's Unicode extensions (`-u-fw-mon`, `-u-mu-celsius`, `-u-hc-h23`, which
 * Android 14's regional preferences write) are formatting choices too, so they come along
 * with the region.
 */
object FormattingLocale {
    /**
     * [hasData] answers whether the platform has formatting data for a locale in its own
     * region, rather than silently falling back to its bare language; [availableIn] builds
     * one from the platform's available-locale list.
     */
    fun of(
        language: Locale,
        system: Locale,
        hasData: (Locale) -> Boolean,
    ): Locale {
        if (system.country.isEmpty()) {
            return withExtensions(language, system)
        }
        val paired = withExtensions(Locale.Builder().setLocale(language).setRegion(system.country).build(), system)
        return if (hasData(paired)) paired else system
    }

    /**
     * Whether [available] (e.g. `DateFormat.getAvailableLocales()`) holds data for a locale's
     * language in its region: the same language and region, and the same script where both
     * name one. A locale without a region always has data, its language's own.
     */
    fun availableIn(available: Array<Locale>): (Locale) -> Boolean {
        val pairs = available.filter { it.country.isNotEmpty() }
        return { locale ->
            locale.country.isEmpty() ||
                pairs.any {
                    it.language == locale.language &&
                        it.country == locale.country &&
                        (it.script.isEmpty() || locale.script.isEmpty() || it.script == locale.script)
                }
        }
    }

    private fun withExtensions(
        locale: Locale,
        system: Locale,
    ): Locale {
        val builder = Locale.Builder().setLocale(locale)
        for (key in system.unicodeLocaleKeys) {
            builder.setUnicodeLocaleKeyword(key, system.getUnicodeLocaleType(key))
        }
        return builder.build()
    }
}
