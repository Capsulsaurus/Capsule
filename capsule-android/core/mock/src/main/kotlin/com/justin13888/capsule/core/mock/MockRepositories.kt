package com.justin13888.capsule.core.mock

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
import com.justin13888.capsule.core.mock.image.ImageMemoryCache
import com.justin13888.capsule.core.mock.image.ProceduralImageSource
import com.justin13888.capsule.core.mock.image.ProceduralImages
import com.justin13888.capsule.core.mock.repo.FakeAlbumRepository
import com.justin13888.capsule.core.mock.repo.FakeAssetRepository
import com.justin13888.capsule.core.mock.repo.FakeDropRepository
import com.justin13888.capsule.core.mock.repo.FakeImportRepository
import com.justin13888.capsule.core.mock.repo.FakeMetadataRepository
import com.justin13888.capsule.core.mock.repo.FakeModerationRepository
import com.justin13888.capsule.core.mock.repo.FakePeopleRepository
import com.justin13888.capsule.core.mock.repo.FakeRecoveryRepository
import com.justin13888.capsule.core.mock.repo.FakeSearchRepository
import com.justin13888.capsule.core.mock.repo.FakeSessionRepository
import com.justin13888.capsule.core.mock.repo.FakeSettingsRepository
import com.justin13888.capsule.core.mock.repo.FakeSharingRepository
import com.justin13888.capsule.core.mock.repo.FakeStackRepository
import com.justin13888.capsule.core.mock.repo.FakeStorageRepository
import com.justin13888.capsule.core.mock.repo.FakeSyncRepository
import com.justin13888.capsule.core.mock.repo.FakeTimelineRepository
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import java.time.Clock
import java.time.ZoneOffset

/**
 * Every contract in `core:data`, wired to one backend.
 *
 * A single object rather than sixteen separately constructed fakes, because they are only
 * correct **together**: they share one [MockBackend], and a caller that built two backends by
 * accident would get a caption written through one that the timeline collected from the other
 * never sees. Handing out the whole set makes that mistake impossible to make.
 *
 * Dependency injection is left to the consumer. This module declares no container of its own,
 * so a debug build can register these against the same interfaces the real adapters will
 * eventually claim, and swapping them is a one-line module change rather than a refactor.
 */
@Suppress("LongParameterList")
class MockRepositories private constructor(
    /** The engine every fake reads and writes through. */
    val backend: MockBackend,
    /** Procedural imagery, in the form a Coil 3 fetcher is built on. */
    val images: ProceduralImageSource,
    /** Adding a device, until a real contract owns that flow. */
    val enrollment: EnrollmentGateway,
    /** The timeline geometry the gallery lays out from. */
    val timelineIndex: TimelineIndexSource,
    /** Assets and their own flags. */
    val assets: AssetRepository,
    /** The library listing and the trash. */
    val timeline: TimelineRepository,
    /** Container albums and the default pointer. */
    val albums: AlbumRepository,
    /** Stacks. */
    val stacks: StackRepository,
    /** Captions and tags. */
    val metadata: MetadataRepository,
    /** Search and saved searches. */
    val search: SearchRepository,
    /** People and face clusters. */
    val people: PeopleRepository,
    /** Sessions, devices, and the local-auth gate. */
    val sessions: SessionRepository,
    /** Library preferences. */
    val settings: SettingsRepository,
    /** Share and upload links. */
    val sharing: SharingRepository,
    /** The guest-deposit inbox. */
    val drops: DropRepository,
    /** Import planning and runs. */
    val imports: ImportRepository,
    /** Operator actions and personal blocks. */
    val moderation: ModerationRepository,
    /** Recovery proof and backup artifacts. */
    val recovery: RecoveryRepository,
    /** Quota and cache occupancy. */
    val storage: StorageRepository,
    /** Sync state and the representation ladder. */
    val sync: SyncRepository,
) {
    /** Puts [scenario] in force across every repository at once. */
    fun applyScenario(scenario: MockScenario) = backend.applyScenario(scenario)

    companion object {
        /**
         * Builds the whole set.
         *
         * [clock] defaults to a **fixed** clock rather than a system one. A mock that read the
         * wall clock would place trash retention windows, session expiry and the stale-sync
         * threshold relative to whenever it happened to run, and every screenshot golden that
         * shows a relative time would drift out from under the suite.
         */
        fun create(
            spec: LibrarySpec = LibrarySpec.DEFAULT,
            scenario: MockScenario = MockScenario.Default,
            clock: Clock = Clock.fixed(spec.now, ZoneOffset.UTC),
            imageCache: ImageMemoryCache = ImageMemoryCache(ImageMemoryCache.DEFAULT_BUDGET_BYTES),
        ): MockRepositories = from(MockBackend(SeededLibrary.generate(spec), clock, scenario), imageCache)

        /** Builds the set over an existing backend, for a test that needs to reach the state. */
        fun from(
            backend: MockBackend,
            imageCache: ImageMemoryCache = ImageMemoryCache(ImageMemoryCache.DEFAULT_BUDGET_BYTES),
        ): MockRepositories {
            val timeline = FakeTimelineRepository(backend)
            val albums = FakeAlbumRepository(backend)
            return MockRepositories(
                backend = backend,
                images =
                    ProceduralImages(
                        cache = imageCache,
                        // The sandboxed-decode-failure state, reachable without a rebuild.
                        decodable = { backend.scenario.value.decodeSucceeds },
                    ),
                enrollment = MockEnrollmentGateway(backend),
                timelineIndex = timeline,
                assets = FakeAssetRepository(backend),
                timeline = timeline,
                albums = albums,
                stacks = FakeStackRepository(backend),
                metadata = FakeMetadataRepository(backend),
                search = FakeSearchRepository(backend),
                people = FakePeopleRepository(backend),
                sessions = FakeSessionRepository(backend),
                settings = FakeSettingsRepository(backend),
                sharing = FakeSharingRepository(backend),
                drops = FakeDropRepository(backend),
                imports = FakeImportRepository(backend, albums),
                moderation = FakeModerationRepository(backend),
                recovery = FakeRecoveryRepository(backend),
                storage = FakeStorageRepository(backend),
                sync = FakeSyncRepository(backend),
            )
        }
    }
}
