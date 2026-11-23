package com.justin13888.capsule.feature.sync

import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module
import java.time.Clock

/**
 * Everything `feature:sync` contributes to the app's object graph.
 *
 * The two ports here — peer discovery and the quarantine listing — are bound as singletons
 * because both are seams over surfaces `core:data` does not yet own: an mDNS responder that is
 * explicitly post-v1, and a quarantine contract that does not exist. Naming them in the graph
 * rather than constructing them inside a view model is what makes replacing either a one-line
 * change here instead of a rewrite of three screens.
 *
 * The [Clock] is injected for the same reason it is injected into the mock backend: staleness
 * is "how long since", and a screen that read the wall clock could not be tested at all.
 */
val syncFeatureModule: Module =
    module {
        single<Clock> { Clock.systemUTC() }
        single<PeerDiscovery> { DirectoryPeerDiscovery(get(), get()) }
        single<QuarantineSource> { TimelineQuarantineSource(get()) }

        viewModel { SyncCentreViewModel(get(), get(), get(), get(), get(), get()) }
        viewModel { TransferQueueViewModel(get(), get()) }
        viewModel { QuarantineViewModel(get()) }
    }
