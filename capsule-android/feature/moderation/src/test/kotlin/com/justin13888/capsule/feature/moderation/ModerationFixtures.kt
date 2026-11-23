package com.justin13888.capsule.feature.moderation

import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.ModerationEventId
import com.justin13888.capsule.core.model.ModerationEventKind
import com.justin13888.capsule.core.model.SuspensionEffects
import com.justin13888.capsule.core.model.UserBlock
import com.justin13888.capsule.core.model.UserId
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
 * Hand-built records exist alongside it for the screen tests. A rendering test wants one
 * entry of a chosen shape — a takedown with a stated reason, a legal hold, an unknown kind —
 * and finding one in a generated fixture would make the test's subject depend on the
 * generator's draws.
 */
internal object ModerationFixtures {
    /** The library "now", so every date in a fixture lines up with the seeded one. */
    val NOW: Instant = LibrarySpec.SMALL.now

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

    private val OWNER = UserId("0197a0d0-0000-7000-8000-0000000000aa")
    private val ASSET = AssetId("0197a0d0-0000-7000-8000-000000000001")

    /** An asset that stopped being served, with a reason the deployment chose to publish. */
    val TAKEDOWN: ModerationEvent =
        ModerationEvent(
            id = ModerationEventId("0197a0d0-0000-7000-8000-0000000000f1"),
            userId = OWNER,
            assetId = ASSET,
            kind = ModerationEventKind.Takedown.asWire(),
            reason = "Reported by a recipient of a share link.",
            createdAt = NOW.minusSeconds(SECONDS_PER_DAY * 3),
        )

    /** The indefinite variant, which no administrator may lift at their discretion. */
    val LEGAL_HOLD: ModerationEvent =
        ModerationEvent(
            id = ModerationEventId("0197a0d0-0000-7000-8000-0000000000f2"),
            userId = OWNER,
            assetId = ASSET,
            kind = ModerationEventKind.LegalHold.asWire(),
            createdAt = NOW.minusSeconds(SECONDS_PER_DAY * 2),
        )

    /** An account-level action, which is the only entry shape with no asset. */
    val SUSPENSION: ModerationEvent =
        ModerationEvent(
            id = ModerationEventId("0197a0d0-0000-7000-8000-0000000000f3"),
            userId = OWNER,
            kind = ModerationEventKind.Suspended.asWire(),
            reason = "Repeated reports upheld.",
            createdAt = NOW.minusSeconds(SECONDS_PER_DAY),
        )

    /** The account's standing while suspended, as the design fixes it. */
    val SUSPENDED: SuspensionEffects = SuspensionEffects()

    /** One block, for the row-rendering tests. */
    val BLOCK: UserBlock =
        UserBlock(
            blockedUserId = UserId("0197a0d0-0000-7000-8000-0000000000bb"),
            createdAt = NOW.minusSeconds(SECONDS_PER_DAY * 5),
        )

    /** A settled appeal state over the three hand-built entries. */
    fun appealState(
        caseId: String = TAKEDOWN.id.value,
        events: List<ModerationEvent> = listOf(SUSPENSION, LEGAL_HOLD, TAKEDOWN),
        effects: SuspensionEffects? = SUSPENDED,
        phase: AppealPhase = AppealPhase.Editing,
    ): AppealUiState =
        AppealUiState(
            caseId = caseId,
            isLoading = false,
            failure = null,
            effects = effects,
            events = events,
            phase = phase,
        )

    /** A settled blocked-list state. */
    fun blocksState(blocks: List<UserBlock> = listOf(BLOCK)): BlocksUiState =
        BlocksUiState(isLoading = false, failure = null, blocks = blocks)

    /** A content hash that is the right shape for the subject-kind inference to accept. */
    const val CONTENT_HASH: String = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90"

    private const val SECONDS_PER_DAY = 86_400L
}
