package com.justin13888.capsule.feature.sync

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
 * The clock is fixed at the fixture's own "now", which is the only way the staleness
 * threshold means anything: the fixture places its last-sync timestamps relative to that
 * instant, and a test that read the wall clock would call every library stale forever.
 */
object SyncFixture {
    /** Structure is what these tests assert on, so the small spec is enough. */
    private val SPEC = LibrarySpec.SMALL

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** A clock fixed at the library's own "now". */
    val CLOCK: Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** A repository set over a fresh backend, under [scenario]. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library = LIBRARY, clock = CLOCK, initialScenario = scenario))

    /** A quarantine source that answers with exactly [items]. */
    fun quarantine(vararg items: QuarantineItem): QuarantineSource = QuarantineSource { items.toList() }
}
