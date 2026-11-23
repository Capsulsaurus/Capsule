package com.justin13888.capsule.feature.settings

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.preferencesDataStoreFile
import org.koin.android.ext.koin.androidContext
import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModelOf
import org.koin.dsl.module

/** The file the device-scoped preferences live in. Named once so a rename cannot orphan it. */
private const val PREFERENCES_FILE = "capsule_device_settings"

/**
 * This slice's bindings, declared beside the code they construct.
 *
 * The module is exported rather than registered: the app module owns graph assembly, so it
 * decides when this list joins the graph. Declaring it here keeps the constructor arguments
 * and the binding that satisfies them in one file, which is the only place a mismatch between
 * them can be noticed by reading.
 *
 * The DataStore is a `single` because it must be: a preferences DataStore takes an exclusive
 * lock on its file, and a second instance over the same path throws rather than merging.
 */
val settingsFeatureModule: Module =
    module {
        single<DataStore<Preferences>> {
            PreferenceDataStoreFactory.create(
                produceFile = { androidContext().preferencesDataStoreFile(PREFERENCES_FILE) },
            )
        }
        single<DeviceSettingsRepository> { DataStoreDeviceSettingsRepository(get()) }
        single<LocaleApplier> { PlatformLocaleApplier(androidContext()) }
        single { androidContext().capsuleBuildInfo() }
        viewModelOf(::SettingsViewModel)
    }
