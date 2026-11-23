package com.justin13888.capsule.feature.audit

import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.LifecycleAction
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.ModerationEventId
import com.justin13888.capsule.core.model.ModerationEventKind
import com.justin13888.capsule.core.model.PlatformTag
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.core.model.asWire
import java.time.Clock
import java.time.Instant
import java.time.ZoneOffset

/**
 * The fixtures both halves of this module's suite are built from.
 *
 * The seeded library is generated once and shared. Generation is pure and it is the expensive
 * part, so regenerating per test would buy nothing but a slower suite — and the mutable half
 * is per-test anyway, since each call builds fresh state over it.
 *
 * Hand-built records exist alongside it for the screen tests. A rendering test wants one
 * record of a chosen shape — a create with no prior link, a delete carrying a retention
 * deadline — and finding one in a two-thousand-asset fixture would make the test's subject
 * depend on the generator's draws.
 */
internal object AuditFixtures {
    /** The library "now", so every relative date in a fixture lines up. */
    val NOW: Instant = Instant.parse("2026-06-15T19:00:00Z")

    private val SPEC = LibrarySpec.SMALL

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** The whole repository set over fresh mutable state under [scenario]. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library(), clock(), scenario))

    /** The generated fixture, for a test that needs to reach the backend directly. */
    fun library(): MockLibrary = LIBRARY

    /** A clock fixed at the library's own "now", so relative dates line up with the fixture. */
    fun clock(): Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** This device, as the signed directory carries it. */
    val THIS_DEVICE: Device =
        Device(
            id = DeviceId("11111111-1111-4111-8111-111111111111"),
            displayName = "Pixel 9 Pro",
            platform = PlatformTag.Android.asWire(),
            addedAt = NOW.minusSeconds(SECONDS_PER_DAY * 400),
            isThisDevice = true,
        )

    /** A device that has been retired. Its row is kept so its old signatures stay checkable. */
    val RETIRED_DEVICE: Device =
        Device(
            id = DeviceId("22222222-2222-4222-8222-222222222222"),
            displayName = "Old laptop",
            platform = PlatformTag.Macos.asWire(),
            addedAt = NOW.minusSeconds(SECONDS_PER_DAY * 900),
            revokedAt = NOW.minusSeconds(SECONDS_PER_DAY * 30),
        )

    /** The directory both devices live in. */
    val DIRECTORY: Map<DeviceId, Device> =
        listOf(THIS_DEVICE, RETIRED_DEVICE).associateBy(Device::id)

    private val ASSET = AssetId("0197a0d0-0000-7000-8000-000000000001")

    /** The chain's first link: no prior hash, because nothing precedes a create. */
    val CREATE: AuditEntry.Lifecycle =
        AuditEntry.Lifecycle(
            ProvenanceRecord(
                assetId = ASSET,
                action = LifecycleAction.Create.asWire(),
                priorProvenanceHash = null,
                recordHash = "aaaa1111",
                byDevice = THIS_DEVICE.id,
                at = NOW.minusSeconds(SECONDS_PER_DAY * 10),
            ),
        )

    /** A soft delete, carrying the retention deadline signed into its manifest. */
    val DELETE: AuditEntry.Lifecycle =
        AuditEntry.Lifecycle(
            ProvenanceRecord(
                assetId = ASSET,
                action = LifecycleAction.Delete.asWire(),
                priorProvenanceHash = CREATE.record.recordHash,
                recordHash = "bbbb2222",
                byDevice = RETIRED_DEVICE.id,
                at = NOW.minusSeconds(SECONDS_PER_DAY * 2),
                retentionUntil = NOW.plusSeconds(SECONDS_PER_DAY * 28),
            ),
        )

    /** An operator action against the account's own library. */
    val TAKEDOWN: AuditEntry.Moderation =
        AuditEntry.Moderation(
            ModerationEvent(
                id = ModerationEventId("0197a0d0-0000-7000-8000-0000000000ff"),
                userId = UserId("0197a0d0-0000-7000-8000-0000000000aa"),
                assetId = ASSET,
                kind = ModerationEventKind.Takedown.asWire(),
                reason = "Reported by a recipient of a share link.",
                createdAt = NOW.minusSeconds(SECONDS_PER_DAY),
            ),
        )

    /** A log holding one of each shape, newest first. */
    fun log(
        entries: List<AuditEntry> = listOf(TAKEDOWN, DELETE, CREATE),
        truncated: Boolean = false,
        scannedAssets: Int = 1,
    ): AuditLog =
        AuditLog(
            entries = entries.sortedByDescending(AuditEntry::at),
            scannedAssets = scannedAssets,
            truncated = truncated,
        )

    /** A settled content state over [log]. */
    fun state(
        log: AuditLog = log(),
        filter: AuditFilter = AuditFilter(),
        selectedId: String? = null,
    ): AuditUiState =
        AuditUiState(
            isLoading = false,
            failure = null,
            log = log,
            devices = DIRECTORY,
            filter = filter,
            selectedId = selectedId,
        )

    private const val SECONDS_PER_DAY = 86_400L
}
