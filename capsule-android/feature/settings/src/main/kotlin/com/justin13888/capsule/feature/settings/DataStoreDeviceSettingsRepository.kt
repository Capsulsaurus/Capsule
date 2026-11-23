package com.justin13888.capsule.feature.settings

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/**
 * The device preferences, persisted to a preferences DataStore.
 *
 * DataStore rather than `SharedPreferences` for the property this screen depends on: a write
 * completes before the suspending call returns, so the screen's "saved" is the same moment as
 * the file's, and a crash immediately after a toggle cannot silently lose it.
 *
 * Every read is written defensively. A preferences file is user-visible state that survives an
 * app downgrade, so a value outside the current enum is not an impossible case — it is what a
 * rollback looks like, and falling back to the default beats refusing to open settings at all.
 */
class DataStoreDeviceSettingsRepository(
    private val store: DataStore<Preferences>,
) : DeviceSettingsRepository {
    override fun deviceSettings(): Flow<DeviceSettings> = store.data.map { it.toDeviceSettings() }

    override suspend fun setThemeMode(mode: ThemeMode) {
        store.edit { it[Keys.THEME_MODE] = mode.name }
    }

    override suspend fun setDynamicColor(enabled: Boolean) {
        store.edit { it[Keys.DYNAMIC_COLOR] = enabled }
    }

    override suspend fun setContrast(contrast: CapsuleContrast?) {
        store.edit { preferences ->
            if (contrast == null) preferences.remove(Keys.CONTRAST) else preferences[Keys.CONTRAST] = contrast.name
        }
    }

    override suspend fun setGridAssetsAcross(assetsAcross: Int) {
        require(assetsAcross in CapsuleAppearance.GRID_CHOICES) {
            "$assetsAcross is not a rung of the grid ladder"
        }
        store.edit { it[Keys.GRID_ACROSS] = assetsAcross }
    }

    override suspend fun setReduceMotion(enabled: Boolean) {
        store.edit { it[Keys.REDUCE_MOTION] = enabled }
    }

    override suspend fun setLanguageTag(tag: String?) {
        store.edit { preferences ->
            if (tag == null) preferences.remove(Keys.LANGUAGE_TAG) else preferences[Keys.LANGUAGE_TAG] = tag
        }
    }

    override suspend fun setDiagnosticsConsent(consented: Boolean) {
        store.edit { it[Keys.DIAGNOSTICS] = consented }
    }

    /** The stored keys. Named once so a rename cannot silently orphan a user's preference. */
    private object Keys {
        val THEME_MODE = stringPreferencesKey("appearance.theme_mode")
        val DYNAMIC_COLOR = booleanPreferencesKey("appearance.dynamic_color")
        val CONTRAST = stringPreferencesKey("appearance.contrast")
        val GRID_ACROSS = intPreferencesKey("appearance.grid_across")
        val REDUCE_MOTION = booleanPreferencesKey("appearance.reduce_motion")
        val LANGUAGE_TAG = stringPreferencesKey("language.tag")
        val DIAGNOSTICS = booleanPreferencesKey("privacy.diagnostics_consent")
    }

    private companion object {
        /** Reads a stored record, tolerating anything a downgraded build may have left. */
        fun Preferences.toDeviceSettings(): DeviceSettings {
            val defaults = CapsuleAppearance()
            return DeviceSettings(
                appearance =
                    CapsuleAppearance(
                        themeMode = enumOrNull<ThemeMode>(this[Keys.THEME_MODE]) ?: defaults.themeMode,
                        dynamicColor = this[Keys.DYNAMIC_COLOR] ?: defaults.dynamicColor,
                        contrast = enumOrNull<CapsuleContrast>(this[Keys.CONTRAST]),
                        gridAssetsAcross =
                            this[Keys.GRID_ACROSS]?.takeIf { it in CapsuleAppearance.GRID_CHOICES }
                                ?: defaults.gridAssetsAcross,
                        reduceMotion = this[Keys.REDUCE_MOTION] ?: defaults.reduceMotion,
                    ),
                languageTag = this[Keys.LANGUAGE_TAG]?.takeIf { tag -> CapsuleLanguage.ALL.any { it.tag == tag } },
                diagnosticsConsent = this[Keys.DIAGNOSTICS] ?: false,
            )
        }

        inline fun <reified E : Enum<E>> enumOrNull(name: String?): E? =
            name?.let { stored -> enumValues<E>().firstOrNull { it.name == stored } }
    }
}
