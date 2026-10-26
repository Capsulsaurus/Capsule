package com.justin13888.capsule.i18n

import java.text.DateFormat
import java.text.NumberFormat
import java.util.Calendar
import java.util.Locale
import java.util.TimeZone
import kotlin.test.Test
import kotlin.test.assertEquals

class FormattingLocaleTest {
    private fun tag(tag: String) = Locale.forLanguageTag(tag)

    @Test
    fun takesTheLanguageFromTheAppAndTheRegionFromTheSystem() {
        assertEquals("zh-Hant-GB", FormattingLocale.of(tag("zh-Hant-HK"), tag("en-GB")).toLanguageTag())
    }

    @Test
    fun keepsTheAppRegionWhenTheSystemHasNone() {
        assertEquals("pt-BR", FormattingLocale.of(tag("pt-BR"), tag("en")).toLanguageTag())
    }

    @Test
    fun isTheSystemLocaleWhenNoPerAppLanguageIsSet() {
        assertEquals("fr-CA", FormattingLocale.of(tag("fr-CA"), tag("fr-CA")).toLanguageTag())
    }

    @Test
    fun carriesTheSystemRegionalPreferences() {
        val locale = FormattingLocale.of(tag("ja"), tag("en-US-u-fw-mon-mu-celsius"))
        assertEquals("US", locale.country)
        assertEquals("ja", locale.language)
        assertEquals("mon", locale.getUnicodeLocaleType("fw"))
        assertEquals("celsius", locale.getUnicodeLocaleType("mu"))
    }

    @Test
    fun regionDecidesDateOrderAndNumberGrouping() {
        val date =
            Calendar.getInstance(TimeZone.getTimeZone("UTC")).apply {
                clear()
                set(2026, Calendar.JANUARY, 31)
            }.time

        fun short(locale: Locale) =
            DateFormat.getDateInstance(DateFormat.SHORT, locale).apply { timeZone = TimeZone.getTimeZone("UTC") }.format(date)

        val american = FormattingLocale.of(tag("en"), tag("en-US"))
        val british = FormattingLocale.of(tag("en"), tag("en-GB"))
        assertEquals("1/31/26", short(american))
        assertEquals("31/01/2026", short(british))

        val german = FormattingLocale.of(tag("en"), tag("de-DE"))
        assertEquals("1,234.5", NumberFormat.getInstance(american).format(1234.5))
        assertEquals("1.234,5", NumberFormat.getInstance(german).format(1234.5))
    }
}
