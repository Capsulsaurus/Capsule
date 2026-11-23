package com.justin13888.capsule.feature.recovery

import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.BackupArtifactId
import com.justin13888.capsule.core.model.BackupArtifactSummary
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.RestoreConflict
import com.justin13888.capsule.core.model.RestoreConflictKind
import com.justin13888.capsule.core.model.RestoreReport
import com.justin13888.capsule.core.model.asWire
import java.time.Clock
import java.time.Instant
import java.time.ZoneOffset

/**
 * The fixtures both halves of this module's suite are built from.
 *
 * The seeded library is generated once and shared: generation is pure and it is the expensive
 * part, so regenerating per test would buy nothing but a slower suite — the mutable half is
 * per-test anyway, since each call builds fresh state over it.
 *
 * Hand-built reports exist alongside it for the screen tests. A rendering test wants a report
 * holding one conflict of each kind, and finding that in a two-thousand-asset fixture would
 * make the test's subject depend on the generator's draws.
 */
internal object RecoveryFixtures {
    /** The library "now", so every date in a fixture lines up with the seeded one. */
    val NOW: Instant = LibrarySpec.SMALL.now

    /** The passphrase the seeded escrow accepts. */
    const val CORRECT_SECRET: String = "correct horse battery staple"

    /** Anything else. */
    const val WRONG_SECRET: String = "battery staple horse correct"

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(LibrarySpec.SMALL) }

    /** The generated fixture, for a test that needs to reach the backend directly. */
    fun library(): MockLibrary = LIBRARY

    /** A clock fixed at the library's own "now", so relative dates line up with the fixture. */
    fun clock(): Clock = Clock.fixed(LibrarySpec.SMALL.now, ZoneOffset.UTC)

    /** A fresh backend over the shared library. */
    fun backend(scenario: MockScenario = MockScenario.Default): MockBackend = MockBackend(library(), clock(), scenario)

    /** The whole repository set over fresh mutable state under [scenario]. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library(), clock(), scenario))

    private val EXPORTER = DeviceId("11111111-1111-4111-8111-111111111111")

    /** An archive whose manifest MAC and exporter signature both verify. */
    val ARTIFACT: BackupArtifactSummary =
        BackupArtifactSummary(
            id = BackupArtifactId("0197a0d0-0000-7000-8000-0000000000a1"),
            artifactVersion = 1,
            cryptoSuiteId = 1,
            minProtocolVersion = "2026-01-01",
            exporterDevice = EXPORTER,
            exportedAt = NOW.minusSeconds(SECONDS_PER_DAY * 180),
            entryCount = 1_200,
            totalBytes = 42L * 1024 * 1024 * 1024,
            isAuthenticated = true,
        )

    /** The same archive, tampered with — or exported by a device that has since been revoked. */
    val UNAUTHENTICATED: BackupArtifactSummary = ARTIFACT.copy(isAuthenticated = false)

    private val ADDED = AssetId("0197a0d0-0000-7000-8000-000000000001")
    private val SAME = AssetId("0197a0d0-0000-7000-8000-000000000002")

    /** An asset this device has since edited: the live chain runs on past the archived head. */
    val NEWER_LOCALLY: RestoreConflict =
        RestoreConflict(
            assetId = AssetId("0197a0d0-0000-7000-8000-000000000003"),
            kind = RestoreConflictKind.LiveHeadNewer.asWire(),
            backupHead = "aaaa1111",
            liveHead = "bbbb2222",
        )

    /** An asset deleted after the export. Taking the archived copy would bring it back. */
    val DIVERGENT: RestoreConflict =
        RestoreConflict(
            assetId = AssetId("0197a0d0-0000-7000-8000-000000000004"),
            kind = RestoreConflictKind.Divergent.asWire(),
            backupHead = "cccc3333",
            liveHead = "dddd4444",
        )

    /** A dry-run report holding one of every outcome the ladder can produce. */
    fun report(
        conflicts: List<RestoreConflict> = listOf(NEWER_LOCALLY, DIVERGENT),
        applied: List<AssetId> = emptyList(),
    ): RestoreReport =
        RestoreReport(
            verified = ARTIFACT.entryCount,
            wouldAdd = listOf(ADDED),
            identical = listOf(SAME),
            conflicts = conflicts,
            applied = applied,
        )

    /** A settled restore state that has read its dry run and nothing more. */
    fun dryRunState(
        report: RestoreReport = report(),
        decisions: Map<String, RestoreDecision> = emptyMap(),
    ): BackupRestoreUiState =
        BackupRestoreUiState(
            path = ARCHIVE_PATH,
            stage = RestoreStage.DryRun,
            artifact = ARTIFACT,
            report = report,
            decisions = decisions,
        )

    /** Where a test pretends the archive is. */
    const val ARCHIVE_PATH: String = "/sdcard/Download/capsule-backup.tar"

    private const val SECONDS_PER_DAY = 86_400L
}
