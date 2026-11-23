package com.justin13888.capsule.feature.moderation

import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.core.module.dsl.viewModelOf
import org.koin.dsl.module

/**
 * What this feature contributes to the application graph.
 *
 * Declared here rather than in the app module's DI file so the binding lives beside the thing
 * it binds: a feature that gains a view model should not require an edit two modules away,
 * and seventeen slices editing one file is seventeen conflicts on the same lines.
 *
 * [LocalAppealGateway] is bound to both of the appeal ports and as a `single`. One instance,
 * because the gateway remembers which challenge it issued for which case and a second copy
 * would reject every proof made against the first. The two interfaces stay separate even
 * though one class satisfies both, so that replacing either half — a real transport, a real
 * key store — is a binding change rather than a redesign.
 */
val moderationFeatureModule: Module =
    module {
        single { LocalAppealGateway() }
        single<AppealGateway> { get<LocalAppealGateway>() }
        single<MasterKeyProver> { get<LocalAppealGateway>() }
        viewModel { parameters -> ReportViewModel(get(), parameters.get()) }
        viewModelOf(::BlocksViewModel)
        viewModel { parameters -> AppealViewModel(get(), get(), get(), parameters.get()) }
    }
