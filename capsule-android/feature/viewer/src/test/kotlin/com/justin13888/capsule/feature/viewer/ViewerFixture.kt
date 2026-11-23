package com.justin13888.capsule.feature.viewer

import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibraryLandmarks
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import java.time.Clock
import java.time.ZoneOffset

/**
 * One seeded library, shared by every test that only needs *a* library.
 *
 * Generation is the expensive part and it is pure, so regenerating per test would buy nothing
 * but a slower suite. The mutable half is per-test regardless — each [repositories] call
 * builds a fresh backend over the same immutable fixture — so no test sees another's writes.
 *
 * Everything the degraded-state tests need is a *landmark*: the fixture deliberately forces
 * one asset into each documented state and records where it went. Reaching those states
 * through the landmarks rather than through hand-rolled doubles is what keeps these tests
 * honest about what the repositories actually hand out.
 */
internal object ViewerFixture {
    /** Small enough to generate quickly, large enough to contain every landmark. */
    val SPEC = LibrarySpec.SMALL

    /** The shared immutable fixture. */
    val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** The named asset per documented state. */
    val LANDMARKS: LibraryLandmarks get() = LIBRARY.landmarks

    /** A clock fixed at the library's own "now", so relative dates line up with the fixture. */
    val CLOCK: Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The whole repository set over a fresh mutable state. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library = LIBRARY, clock = CLOCK, initialScenario = scenario))

    /** A viewer opened at [assetId], with no album narrowing the listing. */
    fun viewerViewModel(
        assetId: AssetId,
        repositories: MockRepositories = repositories(),
        contextAlbum: AlbumId? = null,
    ): ViewerViewModel =
        ViewerViewModel(
            openAt = assetId,
            contextAlbum = contextAlbum,
            assets = repositories.assets,
            timeline = repositories.timeline,
            albums = repositories.albums,
            sync = repositories.sync,
            sessions = repositories.sessions,
        )

    /** The first asset a default view would show — an ordinary, undegraded photograph. */
    fun ordinaryAsset(): Asset =
        LIBRARY.assets.first {
            it.isInDefaultViews &&
                it.duration == null &&
                intrinsicDegradations(it).isEmpty() &&
                it.id != LANDMARKS.degradedAssetId
        }

    /** A landmark that must exist in a populated fixture, or a loud failure saying which. */
    fun require(
        id: AssetId?,
        name: String,
    ): AssetId = requireNotNull(id) { "the seeded library has no $name landmark" }
}
