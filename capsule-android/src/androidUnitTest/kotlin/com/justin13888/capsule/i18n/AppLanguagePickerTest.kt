package com.justin13888.capsule.i18n

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AppLanguagePickerTest {
    /** An in-memory store that records every write. */
    private class FakeStore(
        var tag: String? = null,
    ) : AppLanguageStore {
        val writes = mutableListOf<String?>()

        override fun applied(): String? = tag

        override fun apply(tag: String?) {
            writes += tag
            this.tag = tag
        }
    }

    private val supported = listOf("en", "zh-Hans", "zh-Hant", "pt-BR", "de")

    @Test
    fun isOfferedOnlyBelowAndroid13() {
        for (sdk in 26..32) assertTrue(AppLanguagePicker.isNeeded(sdk), "API $sdk")
        assertFalse(AppLanguagePicker.isNeeded(33))
        assertFalse(AppLanguagePicker.isNeeded(36))
    }

    @Test
    fun followsTheSystemUntilALanguageIsChosen() {
        assertNull(AppLanguagePicker(FakeStore(), supported).current())
    }

    @Test
    fun reportsTheChosenLanguageAsTheCatalogSpellsIt() {
        assertEquals("zh-Hant", AppLanguagePicker(FakeStore("zh-hant"), supported).current())
        assertEquals("pt-BR", AppLanguagePicker(FakeStore("pt-BR"), supported).current())
    }

    @Test
    fun treatsALanguageOutsideTheCatalogAsNoChoice() {
        assertNull(AppLanguagePicker(FakeStore("sv"), supported).current())
    }

    @Test
    fun choosingALanguageStoresItOnce() {
        val store = FakeStore()
        val picker = AppLanguagePicker(store, supported)

        assertTrue(picker.choose("de"))
        assertEquals("de", picker.current())
        assertFalse(picker.choose("de"))
        assertEquals(listOf<String?>("de"), store.writes)
    }

    @Test
    fun choosingTheSystemClearsTheChoice() {
        val store = FakeStore("de")
        val picker = AppLanguagePicker(store, supported)

        assertTrue(picker.choose(null))
        assertNull(store.tag)
        assertFalse(picker.choose(null))
        assertEquals(listOf<String?>(null), store.writes)
    }

    @Test
    fun choosingTheSystemClearsALanguageOutsideTheCatalog() {
        val store = FakeStore("sv")
        assertTrue(AppLanguagePicker(store, supported).choose(null))
        assertNull(store.tag)
    }

    @Test
    fun refusesALanguageOutsideTheCatalog() {
        val store = FakeStore()
        assertFailsWith<IllegalArgumentException> { AppLanguagePicker(store, supported).choose("sv") }
        assertTrue(store.writes.isEmpty())
    }

    @Test
    fun namesEachLanguageInItself() {
        assertEquals("English", AppLanguagePicker.endonym("en"))
        assertEquals("Deutsch", AppLanguagePicker.endonym("de"))
        assertEquals("Français", AppLanguagePicker.endonym("fr"))
        assertEquals("日本語", AppLanguagePicker.endonym("ja"))
        assertEquals("Português (Brasil)", AppLanguagePicker.endonym("pt-BR"))
    }
}
