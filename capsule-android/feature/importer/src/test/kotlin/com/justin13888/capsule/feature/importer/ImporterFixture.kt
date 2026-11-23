package com.justin13888.capsule.feature.importer

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
 * Generation is pure and the expensive part, so regenerating it per test would buy nothing
 * but a slower suite. The mutable half is per test regardless: each [repositories] call builds
 * a fresh backend over the same immutable fixture, so no test sees another's writes.
 */
object ImporterFixture {
    /** Structure is what these tests assert on, so the small spec is enough. */
    private val SPEC = LibrarySpec.SMALL

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** A repository set over a fresh backend, under [scenario]. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(
            MockBackend(
                library = LIBRARY,
                clock = Clock.fixed(SPEC.now, ZoneOffset.UTC),
                initialScenario = scenario,
            ),
        )

    /** A probe that always reports [bytes] free. */
    fun freeSpace(bytes: Long): FreeSpaceProbe = FreeSpaceProbe { bytes }

    /** A scanner that yields exactly [count] files under whatever source it is handed. */
    fun scanner(count: Int): SourceScanner =
        SourceScanner { source ->
            (1..count).map { index -> "${source.locator}/IMG_$index.JPG" }
        }
}
