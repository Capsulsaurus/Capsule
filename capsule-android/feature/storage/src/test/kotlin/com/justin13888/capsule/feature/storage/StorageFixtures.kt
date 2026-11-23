package com.justin13888.capsule.feature.storage

import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.QuotaState
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncState
import java.time.Clock
import java.time.Instant
import java.time.ZoneOffset

/**
 * The fixtures both halves of this module's suite are built from.
 *
 * The seeded library is generated once and shared: generation is pure and it is the expensive
 * part, while the mutable half is per-test anyway because each call builds fresh state over
 * it. Quota states come from the named [MockScenario] presets rather than from hand-built
 * [com.justin13888.capsule.core.model.QuotaStatus] values, so a test asserts one of the app's
 * documented states rather than a number this file invented.
 */
internal object StorageFixtures {
    private val SPEC = LibrarySpec.SMALL

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** The generated fixture, for a test that builds its own backend. */
    fun library(): MockLibrary = LIBRARY

    /** A backend over fresh mutable state under [scenario]. */
    fun backend(scenario: MockScenario = MockScenario.Default): MockBackend =
        MockBackend(library = LIBRARY, clock = clock(), initialScenario = scenario)

    /** The whole repository set over fresh mutable state under [scenario]. */
    fun repositories(scenario: MockScenario): MockRepositories = MockRepositories.from(backend(scenario))

    /** A clock fixed at the library's own "now", so every relative date lines up. */
    fun clock(): Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The library's "now". */
    val NOW: Instant get() = SPEC.now

    /**
     * Fetches one asset's original so a release test has something to release.
     *
     * The seeded library deliberately holds no originals — a cache under real pressure keeps
     * previews and thumbnails, which is what the fixture models — so the tier is fetched
     * through the ordinary ladder rather than written into the fixture. That also keeps the
     * chosen asset one the server considers durable, which is the state the gate is supposed
     * to say yes to.
     */
    suspend fun holdOriginal(repositories: MockRepositories): AssetId {
        val asset =
            repositories.timeline
                .timelineWindow(0, StorageSource.CONSUMER_SCAN)
                .first { it.syncState == SyncState.Synced && it.dimensions != null }
        repositories.sync.ensureRepresentation(asset.id, RepresentationTier.Original)
        return asset.id
    }

    /** One consumer row of a chosen shape, for a rendering test. */
    fun consumer(
        id: String = "asset-original",
        bytes: Long = 8L * 1024 * 1024,
        tiers: Set<RepresentationTier> = setOf(RepresentationTier.Lqip, RepresentationTier.Original),
        isPinned: Boolean = false,
        release: ReleaseState = ReleaseState.Idle,
    ): StorageConsumer =
        StorageConsumer(
            id = AssetId(id),
            capturedAt = NOW,
            assetType = AssetType.Photo,
            bytes = bytes,
            held = HeldRepresentations(tiers),
            isPinned = isPinned,
            release = release,
        )

    /** A settled state with one original-holding consumer in it. */
    fun state(
        standing: QuotaStanding,
        consumers: List<StorageConsumer> = listOf(consumer()),
    ): StorageUiState =
        StorageUiState(
            isLoading = false,
            standing = standing,
            cacheBytes = 3L * 1024 * 1024 * 1024,
            consumers = consumers,
        )

    /** The standing one scenario puts the account in, without a repository round trip. */
    fun standing(state: QuotaState): QuotaStanding =
        QuotaStanding(
            state = state,
            used = 90L * 1024 * 1024 * 1024,
            softLimit = 80L * 1024 * 1024 * 1024,
            hardLimit = 100L * 1024 * 1024 * 1024,
            graceRemaining = null,
        )
}
