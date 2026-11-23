package com.justin13888.capsule.feature.search

import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module

/**
 * What this feature contributes to the application graph.
 *
 * Declared here rather than in the app module's DI file so the binding lives beside the
 * thing it binds: a feature that gains a view model should not require an edit two modules
 * away, and seventeen slices editing one file is seventeen conflicts on the same lines.
 *
 * Only the view model. Every repository it needs is a `core:data` contract the app module
 * already binds, which is what keeps this module free of any knowledge that its data is
 * currently synthetic.
 */
val searchFeatureModule: Module =
    module {
        viewModel { SearchViewModel(get(), get(), get(), get(), get()) }
    }
