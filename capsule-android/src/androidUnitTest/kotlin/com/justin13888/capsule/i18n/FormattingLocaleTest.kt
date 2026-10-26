package com.justin13888.capsule.i18n

import java.text.DateFormat
import java.text.NumberFormat
import java.util.Calendar
import java.util.Locale
import java.util.TimeZone
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class FormattingLocaleTest {
    private fun tag(tag: String) = Locale.forLanguageTag(tag)

    /** A platform that has data for exactly [tags]. */
    private fun dataFor(vararg tags: String) = FormattingLocale.availableIn(tags.map(::tag).toTypedArray())

    /** The JVM's own data, as the app reads Android's. */
    private val jvm = FormattingLocale.availableIn(DateFormat.getAvailableLocales())

    @Test
    fun takesTheLanguageFromTheAppAndTheRegionFromTheSystemWhereThePairHasData() {
        val hasData = dataFor("zh-Hant-HK", "zh-TW", "en-GB", "en-DE")
        assertEquals("zh-Hant-TW", FormattingLocale.of(tag("zh-Hant-HK"), tag("zh-TW"), hasData).toLanguageTag())
        assertEquals("en-DE", FormattingLocale.of(tag("en"), tag("de-DE"), hasData).toLanguageTag())
    }

    @Test
    fun usesTheSystemLocaleWhereThePairHasNoData() {
        val hasData = dataFor("zh-Hant-HK", "en-GB", "en-US", "de-DE")
        assertEquals("en-GB", FormattingLocale.of(tag("zh-Hant-HK"), tag("en-GB"), hasData).toLanguageTag())
        assertEquals("en-US", FormattingLocale.of(tag("fr"), tag("en-US"), hasData).toLanguageTag())
        assertEquals("de-DE", FormattingLocale.of(tag("ja"), tag("de-DE"), hasData).toLanguageTag())
    }

    @Test
    fun keepsTheAppRegionWhenTheSystemHasNone() {
        assertEquals("pt-BR", FormattingLocale.of(tag("pt-BR"), tag("en"), dataFor()).toLanguageTag())
    }

    @Test
    fun isTheSystemLocaleWhenNoPerAppLanguageIsSet() {
        assertEquals("fr-CA", FormattingLocale.of(tag("fr-CA"), tag("fr-CA"), dataFor("fr-CA")).toLanguageTag())
    }

    @Test
    fun carriesTheSystemRegionalPreferences() {
        val paired = FormattingLocale.of(tag("en"), tag("en-US-u-fw-mon-mu-celsius"), dataFor("en-US"))
        assertEquals("US", paired.country)
        assertEquals("mon", paired.getUnicodeLocaleType("fw"))
        assertEquals("celsius", paired.getUnicodeLocaleType("mu"))

        val fallback = FormattingLocale.of(tag("ja"), tag("en-US-u-fw-mon-mu-celsius"), dataFor("en-US"))
        assertEquals("en", fallback.language)
        assertEquals("US", fallback.country)
        assertEquals("mon", fallback.getUnicodeLocaleType("fw"))
    }

    @Test
    fun matchesScriptsOnlyWhereBothNameOne() {
        val hasData = dataFor("zh-Hant-HK", "zh-TW")
        assertTrue(hasData(tag("zh-Hant-TW")))
        assertTrue(hasData(tag("zh-HK")))
        assertFalse(hasData(tag("zh-Hans-HK")))
        assertFalse(hasData(tag("zh-Hant-GB")))
        assertTrue(hasData(tag("zh-Hant")))
    }

    @Test
    fun regionDecidesDateOrderAndNumberGroupingForEveryLanguage() {
        val date =
            Calendar.getInstance(TimeZone.getTimeZone("UTC")).apply {
                clear()
                set(2026, Calendar.JANUARY, 31)
            }.time

        fun short(locale: Locale) =
            DateFormat.getDateInstance(DateFormat.SHORT, locale).apply { timeZone = TimeZone.getTimeZone("UTC") }.format(date)

        fun grouped(locale: Locale) = NumberFormat.getInstance(locale).format(1234567.5)

        assertEquals("1/31/26", short(FormattingLocale.of(tag("en"), tag("en-US"), jvm)))
        assertEquals("31/01/2026", short(FormattingLocale.of(tag("en"), tag("en-GB"), jvm)))
        // There is no zh-Hant-GB data: without the fallback this is the zh-Hant order.
        assertEquals("31/01/2026", short(FormattingLocale.of(tag("zh-Hant-HK"), tag("en-GB"), jvm)))

        assertEquals("1.234.567,5", grouped(FormattingLocale.of(tag("en"), tag("de-DE"), jvm)))
        // There is no de-US or ja-DE data: without the fallback these follow de and ja.
        assertEquals("1,234,567.5", grouped(FormattingLocale.of(tag("de"), tag("en-US"), jvm)))
        assertEquals("1.234.567,5", grouped(FormattingLocale.of(tag("ja"), tag("de-DE"), jvm)))
    }
}
