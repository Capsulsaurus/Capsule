package com.justin13888.capsule.feature.people

import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module

/**
 * What this feature contributes to the application graph.
 *
 * Declared here rather than in the app module's DI file so the binding lives beside the thing
 * it binds: a feature that gains a view model should not require an edit two modules away,
 * and seventeen slices editing one file is seventeen conflicts on the same lines.
 *
 * The person screen's view model takes its id as a Koin parameter rather than reading a
 * saved-state handle, which keeps it constructible from a test with one argument and keeps
 * the navigation key's `String` from leaking any further than the route.
 */
val peopleFeatureModule: Module =
    module {
        viewModel { PeopleViewModel(get(), get(), get(), get()) }
        viewModel { parameters -> PersonDetailViewModel(parameters.get(), get(), get()) }
    }
