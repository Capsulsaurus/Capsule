package com.justin13888.capsule.i18n

import java.util.Locale
import java.util.logging.Logger

/**
 * Where the app's language choice is kept: one BCP 47 tag, or none when the app follows the
 * system language. The choice belongs to this device and is never synced.
 */
interface AppLanguageStore {
    /** The tag the app was given, or null when it follows the system language. */
    fun applied(): String?

    /** Gives the app [tag], or returns it to the system language when [tag] is null. */
    fun apply(tag: String?)
}

/**
 * The in-app language choice, for the Android releases that have no per-app language setting.
 *
 * Android 13+ lets the user pick Capsule's language in system settings from the languages
 * `localeConfig` declares, so the app offers no picker there. Below it, this picker offers the
 * same languages ([supported], in catalog order) plus the system language. Only the language
 * changes: the region stays the system's, which [FormattingLocale] reads.
 */
class AppLanguagePicker(
    private val store: AppLanguageStore,
    /** The catalog's languages as BCP 47 tags, from `locales_config.xml`. */
    val supported: List<String>,
    private val log: Logger = Logger.getLogger("capsule.i18n"),
) {
    /** The chosen language among [supported], or null when the app follows the system. */
    fun current(): String? {
        val applied = store.applied() ?: return null
        return supported.firstOrNull { it.equals(applied, ignoreCase = true) }
    }

    /**
     * Gives the app [tag] (one of [supported]), or the system language when it is null.
     * Returns whether anything changed, so the caller knows whether to re-render.
     */
    fun choose(tag: String?): Boolean {
        require(tag == null || tag in supported) { "unsupported app language: $tag" }
        val before = store.applied()
        if (before.equals(tag, ignoreCase = true)) {
            log.fine { "app language unchanged: language=${tag ?: SYSTEM}" }
            return false
        }
        store.apply(tag)
        log.info { "app language chosen: from=${before ?: SYSTEM} to=${tag ?: SYSTEM}" }
        return true
    }

    companion object {
        /** Android 13 (API 33), where the system offers a per-app language. */
        const val PER_APP_LANGUAGE_SDK = 33

        private const val SYSTEM = "system"

        /** Whether the app has to offer its own picker on this Android release. */
        fun isNeeded(sdkInt: Int): Boolean = sdkInt < PER_APP_LANGUAGE_SDK

        /**
         * A language's name in that language (`Deutsch`, `日本語`), which is how a picker
         * lists it so a reader of any language finds their own. Empty where the platform has
         * no name for it.
         */
        fun endonym(tag: String): String = endonym(Locale.forLanguageTag(tag))

        fun endonym(locale: Locale): String =
            locale.getDisplayName(locale).replaceFirstChar {
                if (it.isLowerCase()) it.titlecase(locale) else it.toString()
            }
    }
}
