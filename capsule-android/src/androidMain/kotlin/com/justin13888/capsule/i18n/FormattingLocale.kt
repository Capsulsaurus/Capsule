package com.justin13888.capsule.i18n

import java.util.Locale

/**
 * The locale every formatter is given: the app's language with the system's region.
 *
 * Language and region are two settings. The language is the per-app language the user
 * picked in system settings (Android 13+, declared through `localeConfig`), or the system
 * language when they picked none; it decides which strings are shown. The region is always
 * the system's, and it decides how dates, times and numbers are written. Choosing Hong Kong
 * Chinese for Capsule on a phone set to British English therefore yields `zh-Hant-GB`:
 * Chinese month names in British day-month order.
 *
 * The system locale's Unicode extensions (`-u-fw-mon`, `-u-mu-celsius`, `-u-hc-h23`, which
 * Android 14's regional preferences write) are formatting choices too, so they come along
 * with the region.
 */
object FormattingLocale {
    fun of(
        language: Locale,
        system: Locale,
    ): Locale {
        val builder = Locale.Builder().setLocale(language)
        if (system.country.isNotEmpty()) {
            builder.setRegion(system.country)
        }
        for (key in system.unicodeLocaleKeys) {
            builder.setUnicodeLocaleKeyword(key, system.getUnicodeLocaleType(key))
        }
        return builder.build()
    }
}
