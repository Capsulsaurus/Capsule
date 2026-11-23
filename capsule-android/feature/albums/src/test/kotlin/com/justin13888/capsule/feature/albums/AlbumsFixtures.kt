package com.justin13888.capsule.feature.albums

import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.ContentHash
import com.justin13888.capsule.core.model.ContentType
import com.justin13888.capsule.core.model.Dimensions
import com.justin13888.capsule.core.model.Lqip
import com.justin13888.capsule.core.model.RgbColor
import com.justin13888.capsule.core.model.asWire
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import java.time.Clock
import java.time.Duration
import java.time.Instant
import java.time.ZoneOffset

/**
 * The seeded library every test in this module runs against.
 *
 * Generated once and shared, because generation is the expensive part and it is pure; the
 * mutable half is per-test regardless, so no test can see another's writes. Driving the real
 * fakes rather than hand-rolled doubles is what keeps these tests honest: a screen that only
 * works against a double is a screen nobody has run.
 */
object AlbumsFixtures {
    /** Structure is what these tests assert on, so the small spec is the right size. */
    val SPEC: LibrarySpec = LibrarySpec.SMALL

    /** The shared immutable fixture. */
    val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** A clock fixed at the library's own "now", so relative dates line up with the fixture. */
    val CLOCK: Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The whole repository set over a backend nothing else has written to. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library = LIBRARY, clock = CLOCK, initialScenario = scenario))

    /**
     * A minimal asset, for the pure tests that are about one field at a time.
     *
     * Hand-built rather than drawn from the fixture where the assertion is about a *value* —
     * "an asset three pixels wide" is clearer written down than found by searching a library.
     */
    @Suppress("LongParameterList")
    fun asset(
        id: String,
        capturedAt: Instant = SPEC.now,
        importedAt: Instant = SPEC.now,
        hash: String = id,
        width: Int = 100,
        height: Int = 100,
        colour: RgbColor? = RgbColor(1, 2, 3),
        rating: Int = 0,
        hidden: Boolean = false,
    ): Asset =
        Asset(
            id = AssetId(id),
            assetType = AssetType.Photo.asWire(),
            captureTimestamp = capturedAt,
            captureUtc = capturedAt,
            captureTzSource = null,
            importTimestamp = importedAt,
            contentHash = ContentHash(hash),
            dimensions = Dimensions(width, height),
            duration = null,
            albumId = null,
            rating = rating,
            isHidden = hidden,
            contentType = ContentType.Jpeg.asWire(),
            lqip = colour?.let { Lqip(chromahash = "", formatVersion = 1, dominantColor = it) },
        )

    /** A day, for tests that step a fixed clock. */
    val DAY: Duration = Duration.ofDays(1)
}

/**
 * Runs a test with a real main dispatcher installed.
 *
 * A view model's own scope dispatches on `Dispatchers.Main`, which does not exist on the JVM
 * until something installs one. Doing it here rather than in every test keeps the reset in a
 * `finally`, so one failing test cannot leave the dispatcher installed for the next.
 */
@OptIn(ExperimentalCoroutinesApi::class)
internal fun viewModelTest(body: suspend TestScope.() -> Unit) =
    runTest {
        Dispatchers.setMain(StandardTestDispatcher(testScheduler))
        try {
            body()
        } finally {
            Dispatchers.resetMain()
        }
    }
