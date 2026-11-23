package com.justin13888.capsule.feature.sharing

import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import java.time.Clock
import java.time.ZoneOffset

/**
 * One generated library shared by every test in this module.
 *
 * Generation is the expensive half of the mock and it is pure, so regenerating per test would
 * buy nothing but a slower suite. The mutable half is per-test regardless: every
 * [freshRepositories] call builds new state over the same immutable fixture, so no test can
 * see another's writes.
 */
internal object SharingFixtures {
    /** Structure is what these tests assert on, so the small fixture is enough. */
    val SPEC: LibrarySpec = LibrarySpec.SMALL

    private val library: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** A clock fixed at the library's own "now", so its relative dates line up. */
    val CLOCK: Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The whole repository set over fresh state. */
    fun freshRepositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library = library, clock = CLOCK, initialScenario = scenario))

    /** The origin every URL assertion in this module is built against. */
    val ORIGIN: LinkOrigin = LinkOrigin("https://capsule.test")
}
