package com.justin13888.capsule.feature.albums

import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.feature.albums.data.AlbumPolicyStore
import com.justin13888.capsule.feature.albums.data.AlbumUpgradeCoordinator
import com.justin13888.capsule.feature.albums.data.DerivedFederatedAlbumSource
import com.justin13888.capsule.feature.albums.data.EmptyHiddenAssetSource
import com.justin13888.capsule.feature.albums.data.FederatedAlbumSource
import com.justin13888.capsule.feature.albums.data.HiddenAssetSource
import com.justin13888.capsule.feature.albums.data.InMemoryAlbumPolicyStore
import com.justin13888.capsule.feature.albums.data.InMemoryAlbumUpgradeCoordinator
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.data.LocalSmartAlbumPreview
import com.justin13888.capsule.feature.albums.data.SmartAlbumPreview
import com.justin13888.capsule.feature.albums.model.CollectionRef
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailViewModel
import com.justin13888.capsule.feature.albums.ui.duplicates.DuplicatesViewModel
import com.justin13888.capsule.feature.albums.ui.hidden.HiddenViewModel
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubViewModel
import com.justin13888.capsule.feature.albums.ui.places.PlacesViewModel
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumBuilderViewModel
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumsViewModel
import com.justin13888.capsule.feature.albums.ui.stacks.StackDetailViewModel
import com.justin13888.capsule.feature.albums.ui.stacks.StacksViewModel
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedViewModel
import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module
import java.time.Clock

/**
 * Everything the collections surfaces register into the application graph.
 *
 * Exposed from the feature rather than declared in the app's DI file, so the slice owns its
 * own bindings: the app graph names this one value and never has to know which ports albums,
 * stacks, trash and the map happen to need.
 *
 * No `Clock` is bound here. A clock is exactly the kind of dependency several features would
 * each want to register, and the first one to do it would fix the type for everyone; the view
 * models that need one take it as a constructor parameter instead, which also lets a test pin
 * an instant without reaching into the graph.
 *
 * The four ports below are feature-local because their contracts do not exist in `core:data`
 * yet — album policy and the upgrade ceremony are MLS group state, an album-group assertion
 * has no server surface at all, and no listing exposes hidden assets. Each is an interface
 * with a default binding, so the day a real source lands the change is one line.
 */
val albumsFeatureModule: Module =
    module {
        single<AlbumPolicyStore> { InMemoryAlbumPolicyStore() }
        single<AlbumUpgradeCoordinator> { InMemoryAlbumUpgradeCoordinator(clock = Clock.systemUTC()) }
        single<FederatedAlbumSource> { DerivedFederatedAlbumSource(albums = get()) }
        single<HiddenAssetSource> { EmptyHiddenAssetSource }

        factory { LibrarySample(timeline = get()) }
        factory<SmartAlbumPreview> { LocalSmartAlbumPreview(sample = get()) }

        viewModel {
            CollectionsHubViewModel(
                albums = get(),
                search = get(),
                federated = get(),
                sample = get(),
                clock = Clock.systemUTC(),
            )
        }
        viewModel { parameters ->
            AlbumDetailViewModel(
                ref = parameters.get<CollectionRef>(),
                albums = get(),
                policies = get(),
                upgrades = get(),
                federated = get(),
                sample = get(),
                clock = Clock.systemUTC(),
            )
        }
        viewModel { SmartAlbumsViewModel(search = get()) }
        viewModel { parameters ->
            SmartAlbumBuilderViewModel(
                smartAlbumId = parameters.getOrNull<SmartAlbumId>(),
                search = get(),
                sessions = get(),
                preview = get(),
                clock = Clock.systemUTC(),
            )
        }
        viewModel { StacksViewModel(stacks = get(), sample = get()) }
        viewModel { parameters ->
            StackDetailViewModel(stackId = parameters.get<StackId>(), stacks = get(), assets = get())
        }
        viewModel { RecentlyDeletedViewModel(timeline = get(), clock = Clock.systemUTC()) }
        viewModel { HiddenViewModel(source = get(), assets = get()) }
        viewModel { DuplicatesViewModel(sample = get(), timeline = get(), clock = Clock.systemUTC()) }
        viewModel { PlacesViewModel(sample = get()) }
    }
