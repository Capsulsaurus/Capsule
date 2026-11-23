package com.justin13888.capsule.feature.timeline

import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import java.time.Clock
import java.time.ZoneOffset

/**
 * The seeded library every test in this module runs against.
 *
 * Generation is the expensive half and it is pure, so the immutable fixture is shared while
 * every test gets its own mutable backend — no test can see another's writes, and the suite
 * stays fast enough that it keeps being run.
 *
 * The clock is fixed at the library's own "now" so a retention deadline asserted in a test is
 * the same deadline on any day of the year.
 */
object TimelineFixture {
    /** Structure is what these tests assert on, so the small fixture is the right one. */
    val SPEC: LibrarySpec = LibrarySpec.SMALL

    /** The instant every relative date in the fixture is measured from. */
    val CLOCK: Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The whole repository set over a fresh backend under [scenario]. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.create(spec = SPEC, scenario = scenario, clock = CLOCK)

    /** A view model over [repositories], with the fixed clock behind its delete deadlines. */
    fun viewModel(mock: MockRepositories): TimelineViewModel =
        TimelineViewModel(
            timeline = mock.timeline,
            index = mock.timelineIndex,
            assets = mock.assets,
            stacks = mock.stacks,
            sync = mock.sync,
            sessions = mock.sessions,
            settings = mock.settings,
            clock = CLOCK,
        )

    /** One landmark asset, or a failure naming which state the fixture failed to place. */
    fun landmark(
        mock: MockRepositories,
        name: String,
        id: AssetId?,
    ): Asset {
        val assetId = requireNotNull(id) { "the fixture placed no $name asset" }
        return requireNotNull(mock.backend.library.assetsById[assetId]) {
            "the $name landmark names an asset the library does not hold"
        }
    }
}
