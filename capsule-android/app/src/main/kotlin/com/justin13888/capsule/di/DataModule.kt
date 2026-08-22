package com.justin13888.capsule.di

import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.DropRepository
import com.justin13888.capsule.core.data.ImportRepository
import com.justin13888.capsule.core.data.MetadataRepository
import com.justin13888.capsule.core.data.ModerationRepository
import com.justin13888.capsule.core.data.PeopleRepository
import com.justin13888.capsule.core.data.RecoveryRepository
import com.justin13888.capsule.core.data.SearchRepository
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.data.SettingsRepository
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.data.StackRepository
import com.justin13888.capsule.core.data.StorageRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.data.TimelineIndexSource
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.mock.MockRepositories
import org.koin.core.module.Module
import org.koin.dsl.module

/**
 * Binds every `core:data` contract to the seeded mock backend.
 *
 * This is the one place the app knows its data is synthetic. Features depend on the
 * interfaces only, so when the UniFFI bindings land this file is what changes — each
 * `get<MockRepositories>().x` becomes the real adapter, and nothing above moves.
 *
 * [MockRepositories] is a single singleton rather than sixteen independently constructed
 * fakes because they are only correct together: they share one backend, so a caption
 * written through the metadata repository is observed by the timeline that collects it.
 * Binding them separately would quietly create two libraries.
 */
val dataModule: Module =
    module {
        single { MockRepositories.create() }

        single<TimelineRepository> { get<MockRepositories>().timeline }
        single<TimelineIndexSource> { get<MockRepositories>().timelineIndex }
        single<AssetRepository> { get<MockRepositories>().assets }
        single<AlbumRepository> { get<MockRepositories>().albums }
        single<StackRepository> { get<MockRepositories>().stacks }
        single<MetadataRepository> { get<MockRepositories>().metadata }
        single<SearchRepository> { get<MockRepositories>().search }
        single<PeopleRepository> { get<MockRepositories>().people }
        single<SessionRepository> { get<MockRepositories>().sessions }
        single<SettingsRepository> { get<MockRepositories>().settings }
        single<SharingRepository> { get<MockRepositories>().sharing }
        single<DropRepository> { get<MockRepositories>().drops }
        single<ImportRepository> { get<MockRepositories>().imports }
        single<ModerationRepository> { get<MockRepositories>().moderation }
        single<RecoveryRepository> { get<MockRepositories>().recovery }
        single<StorageRepository> { get<MockRepositories>().storage }
        single<SyncRepository> { get<MockRepositories>().sync }
    }
