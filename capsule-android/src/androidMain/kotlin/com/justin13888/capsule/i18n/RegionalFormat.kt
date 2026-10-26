package com.justin13888.capsule.i18n

import android.content.Context
import android.content.res.Resources
import java.text.DateFormat
import java.text.NumberFormat
import java.util.Locale

/**
 * Region-driven formatters for the app. Every date, time and number the app shows goes
 * through here rather than through `Locale.getDefault()`, which under a per-app language
 * carries the app language's region instead of the user's.
 */
object RegionalFormat {
    /** The platform's ICU data, read once: which language-region pairs it can format. */
    private val hasData by lazy { FormattingLocale.availableIn(DateFormat.getAvailableLocales()) }

    /** The formatting locale for [context]: its language with the system's region. */
    fun locale(context: Context): Locale =
        FormattingLocale.of(
            language = context.resources.configuration.locales[0],
            // The system resources keep the device's own locale, whatever language the
            // app was given.
            system = Resources.getSystem().configuration.locales[0],
            hasData = hasData,
        )

    fun date(
        context: Context,
        style: Int = DateFormat.MEDIUM,
    ): DateFormat = DateFormat.getDateInstance(style, locale(context))

    fun dateTime(
        context: Context,
        dateStyle: Int = DateFormat.MEDIUM,
        timeStyle: Int = DateFormat.SHORT,
    ): DateFormat = DateFormat.getDateTimeInstance(dateStyle, timeStyle, locale(context))

    fun number(context: Context): NumberFormat = NumberFormat.getInstance(locale(context))
}
