package com.justin13888.capsule.feature.timeline

import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module

/**
 * This feature's own bindings.
 *
 * Declared here rather than in the app's DI file so the module owns its graph: the app
 * assembles modules and never has to know what is inside one. That is also what keeps a feature
 * slice to a single directory — adding a view model here touches no file the shell or any other
 * feature can see.
 *
 * Nothing but the view model. Every repository it needs is already bound against its
 * `core:data` interface by the app's data module, which is the seam the real UniFFI adapters
 * will eventually claim without this file changing at all.
 *
 * Constructed explicitly rather than through the reflection-free `viewModelOf` helper, because
 * the clock is a defaulted parameter: the helper would try to resolve a `Clock` from the graph
 * and fail, where this leaves the default in place and lets a test pass a fixed one.
 */
val timelineFeatureModule: Module =
    module {
        viewModel {
            TimelineViewModel(
                timeline = get(),
                index = get(),
                assets = get(),
                stacks = get(),
                sync = get(),
                sessions = get(),
                settings = get(),
            )
        }
    }
