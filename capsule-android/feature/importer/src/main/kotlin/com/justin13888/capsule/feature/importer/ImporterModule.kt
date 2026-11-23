package com.justin13888.capsule.feature.importer

import com.justin13888.capsule.core.model.ImportId
import org.koin.android.ext.koin.androidContext
import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module

/**
 * Everything `feature:importer` contributes to the app's object graph.
 *
 * The two stores are **singletons** and the three view models are not, which is the whole
 * shape of this feature: a remembered destination and a plan awaiting confirmation outlive the
 * screen that produced them — the picker asks the question, the confirm screen reads the plan,
 * and the run screen names the files — while each screen's state machine dies with its screen.
 */
val importerFeatureModule: Module =
    module {
        single<ScopeMappingStore> { InMemoryScopeMappingStore() }
        single<ImportPlanStore> { InMemoryImportPlanStore() }
        single<FreeSpaceProbe> { AndroidFreeSpaceProbe(androidContext()) }
        single<SourceScanner> { DeterministicSourceScanner() }

        viewModel { ImportSourcesViewModel(get(), get(), get(), get(), get()) }
        viewModel { (importId: ImportId) -> ImportPlanViewModel(get(), get(), get(), get(), get(), importId) }
        viewModel { (importId: ImportId) -> ImportRunViewModel(get(), get(), get(), importId) }
    }
