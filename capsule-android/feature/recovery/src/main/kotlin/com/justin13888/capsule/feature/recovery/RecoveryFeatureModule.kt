package com.justin13888.capsule.feature.recovery

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
 * [InMemoryShamirShareStore] is a `single` and that is load-bearing rather than an
 * optimisation: the guardian-share screen writes it and the guided re-wrap invalidates it, so
 * two instances would mean a re-wrap that left the old, now-useless shares still listed on
 * the other screen — the precise failure the design says must be surfaced rather than hidden.
 */
val recoveryFeatureModule: Module =
    module {
        single<ShamirShareStore> { InMemoryShamirShareStore() }
        single<RecoveryRewrapGateway> { LocalRecoveryRewrapGateway(shares = get()) }
        viewModelOf(::RecoveryStatusViewModel)
        viewModelOf(::RecoveryVerifyViewModel)
        viewModel { RecoveryRewrapViewModel(gateway = get()) }
        viewModel { ShamirViewModel(store = get(), gateway = get()) }
        viewModelOf(::BackupExportViewModel)
        viewModelOf(::BackupRestoreViewModel)
    }
