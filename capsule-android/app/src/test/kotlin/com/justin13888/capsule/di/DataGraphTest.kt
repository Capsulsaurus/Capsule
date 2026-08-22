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
import org.junit.After
import org.junit.Assert.assertSame
import org.junit.Test
import org.koin.core.context.startKoin
import org.koin.core.context.stopKoin
import kotlin.reflect.KClass

/**
 * Every `core:data` contract a feature can inject must actually resolve, and they must all
 * come from one backend.
 *
 * Both halves matter. Koin resolves lazily, so a contract nothing has injected *yet* would
 * otherwise fail on the day the feature that needs it lands rather than here. And the fakes
 * are only correct together: if two of them were built over separate backends, a caption
 * written through one would never appear in the timeline collected from the other, and the
 * app would look subtly, silently wrong rather than broken.
 */
class DataGraphTest {
    @After
    fun stopTheGraph() {
        stopKoin()
    }

    private val contracts: List<KClass<*>> =
        listOf(
            TimelineRepository::class,
            TimelineIndexSource::class,
            AssetRepository::class,
            AlbumRepository::class,
            StackRepository::class,
            MetadataRepository::class,
            SearchRepository::class,
            PeopleRepository::class,
            SessionRepository::class,
            SettingsRepository::class,
            SharingRepository::class,
            DropRepository::class,
            ImportRepository::class,
            ModerationRepository::class,
            RecoveryRepository::class,
            StorageRepository::class,
            SyncRepository::class,
        )

    @Test
    fun every_repository_contract_resolves() {
        val koin = startKoin { modules(dataModule) }.koin
        contracts.forEach { contract ->
            // get() throws with the missing definition named, which is the useful failure.
            koin.get<Any>(contract, null, null)
        }
    }

    @Test
    fun every_repository_shares_one_backend() {
        val koin = startKoin { modules(dataModule) }.koin
        val repositories = koin.get<MockRepositories>()
        assertSame(repositories.timeline, koin.get<TimelineRepository>())
        assertSame(repositories.albums, koin.get<AlbumRepository>())
        assertSame(repositories.metadata, koin.get<MetadataRepository>())
        // The aggregate itself is a singleton, so nothing can accidentally build a second
        // library by resolving the set twice.
        assertSame(repositories, koin.get<MockRepositories>())
    }
}
