package com.justin13888.capsule.feature.storage

import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module

/**
 * This slice's bindings, declared beside the code they construct.
 *
 * Exported rather than registered: the app module owns graph assembly, so it decides when
 * this list joins the graph. Declaring it here keeps the constructor arguments and the
 * binding that satisfies them in one file, which is the only place a mismatch between them
 * can be noticed by reading.
 *
 * [StorageSource] is a `single` and the view model is not. The source is a pure derivation
 * over three repositories and holds nothing, so one instance costs nothing and saves
 * rebuilding it per destination; the view model holds the cache budget, the pin record and
 * the release verdicts, all of which belong to a screen and must not outlive it.
 */
val storageFeatureModule: Module =
    module {
        single { StorageSource(storage = get(), sync = get(), timeline = get()) }
        // Constructed by hand rather than through `viewModelOf`, because the clock carries a
        // default the graph has no binding for: a reflective constructor reference would ask
        // Koin for a `Clock` and fail at first resolution instead of at compile time.
        viewModel { StorageViewModel(source = get(), storage = get(), sync = get()) }
    }
