package com.justin13888.capsule.i18n

import android.content.Context
import androidx.appcompat.app.AppCompatDelegate
import androidx.core.os.LocaleListCompat
import com.justin13888.capsule.R
import org.xmlpull.v1.XmlPullParser

/**
 * The app language kept by AppCompat. Below Android 13 AppCompat applies it to every
 * AppCompat activity and, with `autoStoreLocales` in the manifest, keeps it in this device's
 * app storage; on Android 13+ it is the system per-app language.
 */
object AppCompatLanguageStore : AppLanguageStore {
    override fun applied(): String? =
        AppCompatDelegate
            .getApplicationLocales()
            .takeUnless { it.isEmpty }
            ?.get(0)
            ?.toLanguageTag()

    override fun apply(tag: String?) {
        AppCompatDelegate.setApplicationLocales(
            if (tag == null) LocaleListCompat.getEmptyLocaleList() else LocaleListCompat.forLanguageTags(tag),
        )
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
