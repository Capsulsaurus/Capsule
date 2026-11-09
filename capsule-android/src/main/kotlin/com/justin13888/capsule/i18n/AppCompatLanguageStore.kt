package com.justin13888.capsule.i18n

import android.content.Context
import androidx.appcompat.app.AppCompatDelegate
import androidx.core.os.LocaleListCompat
import com.justin13888.capsule.R
import org.xmlpull.v1.XmlPullParser
import java.util.logging.Logger

/**
 * The app language kept by AppCompat. Below Android 13 AppCompat applies it to every
 * AppCompat activity and, with `autoStoreLocales` in the manifest, keeps it in this device's
 * app storage; on Android 13+ it is the system per-app language.
 *
 * Below Android 13 AppCompat reads its stored choice back only when an AppCompat activity
 * starts, so a process woken for a broadcast (an alert alarm) sees no choice at all. The
 * store therefore also mirrors the tag into its own preferences, which [notificationLanguage]
 * reads; [onApplied] runs after every change, so surfaces outside an activity (the alerts
 * notification channel) can re-render in the new language.
 */
class AppCompatLanguageStore(
    context: Context,
    private val onApplied: (Context) -> Unit = {},
) : AppLanguageStore {
    private val context = context.applicationContext

    /**
     * AppCompat's choice. Read from an activity, where AppCompat has loaded its stored choice,
     * so it also brings the mirror level with a choice made before the mirror existed.
     */
    override fun applied(): String? {
        val tag = appCompatLanguage()
        if (prefs(context).getString(KEY, null) != tag) mirror(tag)
        return tag
    }

    override fun apply(tag: String?) {
        AppCompatDelegate.setApplicationLocales(
            if (tag == null) LocaleListCompat.getEmptyLocaleList() else LocaleListCompat.forLanguageTags(tag),
        )
        mirror(tag)
        onApplied(context)
    }

    private fun mirror(tag: String?) {
        prefs(context).edit().apply { if (tag == null) remove(KEY) else putString(KEY, tag) }.apply()
        log.fine { "app language mirrored: language=${tag ?: "system"}" }
    }

    companion object {
        private const val PREFS = "capsule.i18n"
        private const val KEY = "app_language"
        private val log = Logger.getLogger("capsule.i18n")

        private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

        private fun appCompatLanguage(): String? =
            AppCompatDelegate
                .getApplicationLocales()
                .takeUnless { it.isEmpty }
                ?.get(0)
                ?.toLanguageTag()

        /**
         * The in-app language as any context can see it, or null when the app follows the
         * system: AppCompat's choice where this process has loaded it, otherwise the mirrored
         * one. Only meaningful below Android 13; on 13+ every context already carries it.
         */
        fun notificationLanguage(context: Context): String? = appCompatLanguage() ?: prefs(context.applicationContext).getString(KEY, null)
    }
}

/** The languages `res/xml/locales_config.xml` declares, generated from `locales/config.json`. */
object LocalesConfig {
    private const val ANDROID_NAMESPACE = "http://schemas.android.com/apk/res/android"

    fun tags(context: Context): List<String> {
        val parser = context.resources.getXml(R.xml.locales_config)
        val tags = mutableListOf<String>()
        try {
            while (parser.next() != XmlPullParser.END_DOCUMENT) {
                if (parser.eventType == XmlPullParser.START_TAG && parser.name == "locale") {
                    parser.getAttributeValue(ANDROID_NAMESPACE, "name")?.let(tags::add)
                }
            }
        } finally {
            parser.close()
        }
        return tags
    }
}
