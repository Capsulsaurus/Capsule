package com.justin13888.capsule.feature.settings

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import org.junit.jupiter.api.io.TempDir
import java.io.File

/**
 * What the device preferences do on disk.
 *
 * Worth its own cases because this is the half of the settings screen that has to survive the
 * process ending, an app downgrade, and a value the current build has never heard of. The
 * defensive reads are the point: a preferences file is user-visible state with a longer life
 * than any one build of the app.
 */
class DataStoreDeviceSettingsRepositoryTest {
    @TempDir
    lateinit var directory: File

    @Test
    fun `an empty store reads as the documented defaults`() =
        runTest {
            val settings = repository().deviceSettings().first()

            assertEquals(DeviceSettings(), settings, "nothing stored must mean nothing chosen")
        }

    @Test
    fun `every appearance field round-trips`() =
        runTest {
            val repository = repository()

            repository.setThemeMode(ThemeMode.Dark)
            repository.setDynamicColor(false)
            repository.setContrast(CapsuleContrast.High)
            repository.setGridAssetsAcross(CapsuleAppearance.GRID_CHOICES.last())
            repository.setReduceMotion(true)

            val appearance = repository.deviceSettings().first().appearance
            assertEquals(ThemeMode.Dark, appearance.themeMode)
            assertEquals(false, appearance.dynamicColor)
            assertEquals(CapsuleContrast.High, appearance.contrast)
            assertEquals(CapsuleAppearance.GRID_CHOICES.last(), appearance.gridAssetsAcross)
            assertTrue(appearance.reduceMotion)
        }

    @Test
    fun `clearing the contrast override restores following the platform`() =
        runTest {
            val repository = repository()
            repository.setContrast(CapsuleContrast.Medium)

            repository.setContrast(null)

            assertNull(
                repository
                    .deviceSettings()
                    .first()
                    .appearance.contrast,
                "no override is the absence of a choice, not a choice of Standard",
            )
        }

    @Test
    fun `the language and diagnostics consent round-trip, and clear`() =
        runTest {
            val repository = repository()

            repository.setLanguageTag("ja")
            repository.setDiagnosticsConsent(true)
            assertEquals("ja", repository.deviceSettings().first().languageTag)
            assertTrue(repository.deviceSettings().first().diagnosticsConsent)

            repository.setLanguageTag(null)
            assertNull(repository.deviceSettings().first().languageTag)
        }

    @Test
    fun `a stored value this build does not understand falls back rather than failing`() =
        runTest {
            val store =
                PreferenceDataStoreFactory.create(
                    produceFile = { File(directory, "downgraded.preferences_pb") },
                )
            // What a downgrade looks like: a newer build wrote a constant this one has not got.
            store.edit { it[stringPreferencesKey("appearance.theme_mode")] = "Sepia" }

            val settings = DataStoreDeviceSettingsRepository(store).deviceSettings().first()

            assertEquals(ThemeMode.System, settings.appearance.themeMode)
        }

    @Test
    fun `a stored language the catalogue no longer ships is dropped`() =
        runTest {
            val store =
                PreferenceDataStoreFactory.create(
                    produceFile = { File(directory, "retired-language.preferences_pb") },
                )
            store.edit { it[stringPreferencesKey("language.tag")] = "xx-Retired" }

            val settings = DataStoreDeviceSettingsRepository(store).deviceSettings().first()

            assertNull(settings.languageTag, "a language with no catalogue behind it is not a choice")
        }

    @Test
    fun `a grid density off the ladder is refused at the call rather than stored`() =
        runTest {
            val repository = repository()

            assertThrows<IllegalArgumentException>("the ladder is the closed set of rungs") {
                repository.setGridAssetsAcross(OFF_LADDER)
            }
        }

    private fun repository(): DataStoreDeviceSettingsRepository =
        DataStoreDeviceSettingsRepository(
            PreferenceDataStoreFactory.create(
                produceFile = { File(directory, "device.preferences_pb") },
            ),
        )

    private companion object {
        /** A density no rung of the ladder offers. */
        const val OFF_LADDER = 42
    }
}
