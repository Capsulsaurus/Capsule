package com.justin13888.capsule.alerts

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class AlertNotificationLocaleTest {
    private val system = listOf("en-US", "fr-FR")

    @Test
    fun putsTheChosenLanguageFirstBelowAndroid13() {
        for (sdk in 26..32) {
            assertEquals(listOf("de", "en-US", "fr-FR"), AlertNotificationLocale.select(sdk, "de", system), "API $sdk")
        }
    }

    @Test
    fun configuresNothingOnAndroid13AndLater() {
        assertNull(AlertNotificationLocale.select(33, "de", system))
        assertNull(AlertNotificationLocale.select(36, "de", system))
    }

    @Test
    fun configuresNothingWhenTheAppFollowsTheSystem() {
        assertNull(AlertNotificationLocale.select(30, null, system))
        assertNull(AlertNotificationLocale.select(30, "", system))
    }

    @Test
    fun doesNotRepeatTheChosenLanguageAmongTheFallbacks() {
        assertEquals(listOf("fr-FR", "en-US"), AlertNotificationLocale.select(30, "fr-FR", system))
        assertEquals(listOf("en-us", "fr-FR"), AlertNotificationLocale.select(30, "en-us", system))
    }

    @Test
    fun keepsTheChosenLanguageWithNoSystemLocales() {
        assertEquals(listOf("zh-Hant"), AlertNotificationLocale.select(26, "zh-Hant", emptyList()))
    }
}
