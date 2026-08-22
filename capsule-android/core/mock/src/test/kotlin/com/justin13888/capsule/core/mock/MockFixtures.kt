package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import java.time.Clock
import java.time.ZoneOffset

/**
 * One generated library shared by every test that only needs *a* library.
 *
 * Generation is the expensive part of this module and it is pure, so regenerating it per test
 * would buy nothing but a slower suite — and a slow suite is one that stops being run. The
 * mutable half is per-test regardless: each [freshRepositories] call builds a new
 * [MockState][com.justin13888.capsule.core.mock.MockState] over the same immutable fixture,
 * so no test can see another's writes.
 */
object MockFixtures {
    /** The spec used everywhere structure is what matters and scale is not. */
    val SPEC = LibrarySpec.SMALL

    /** The shared immutable fixture. */
    val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** A clock fixed at the library's own "now", so relative dates line up with the fixture. */
    val CLOCK: Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** A backend with its own mutable state, under [scenario]. */
    fun freshBackend(scenario: MockScenario = MockScenario.Default): MockBackend =
        MockBackend(library = LIBRARY, clock = CLOCK, initialScenario = scenario)

    /** The whole repository set over a fresh backend. */
    fun freshRepositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(
            freshBackend(scenario),
        )
}
