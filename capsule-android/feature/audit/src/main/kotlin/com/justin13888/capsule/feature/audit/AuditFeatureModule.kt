package com.justin13888.capsule.feature.audit

import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModelOf
import org.koin.dsl.module

/**
 * This slice's bindings, declared beside the code they construct.
 *
 * The module is exported rather than registered: the app module owns graph assembly, so it
 * decides when this list joins the graph. Declaring it here keeps the constructor arguments
 * and the binding that satisfies them in the same file, which is the only place a mismatch
 * between them can be noticed by reading.
 *
 * [AuditLogSource] is a `single` and the view model is not. The source holds no state of its
 * own — it is a pure derivation over four repositories — so sharing one instance costs
 * nothing and saves rebuilding it per screen; the view model holds the filter and the
 * selection, which belong to a screen and must not outlive it.
 */
val auditFeatureModule: Module =
    module {
        single {
            AuditLogSource(
                timeline = get(),
                assets = get(),
                moderation = get(),
                sessions = get(),
            )
        }
        viewModelOf(::AuditViewModel)
    }
