package com.justin13888.capsule.feature.security

import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceCohort
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.PlatformTag
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.SessionId
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.core.model.asWire
import java.time.Clock
import java.time.Duration
import java.time.Instant
import java.time.ZoneOffset

/**
 * The fixtures this module's suite is built from.
 *
 * The seeded library is generated once and shared, because generation is pure and expensive
 * while the mutable half is per-test anyway. Hand-built sessions live alongside it for the
 * grouping tests: a grouping test wants two sessions on one cohort and one on another, and
 * finding that shape in a generated ledger would make the test's subject depend on the
 * generator's draws.
 */
internal object SecurityFixtures {
    private val SPEC = LibrarySpec.SMALL

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** The library's "now". */
    val NOW: Instant get() = SPEC.now

    /** A backend over fresh mutable state under [scenario]. */
    fun backend(scenario: MockScenario = MockScenario.Default): MockBackend =
        MockBackend(library = LIBRARY, clock = clock(), initialScenario = scenario)

    /** The whole repository set over fresh mutable state under [scenario]. */
    fun repositories(scenario: MockScenario): MockRepositories = MockRepositories.from(backend(scenario))

    /** A clock fixed at the library's own "now", so every relative date lines up. */
    fun clock(): Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The account everything below belongs to. */
    val OWNER: UserId = UserId("01234567-89ab-7def-8000-00000000000a")

    /** A hash for the device this process is running on. */
    const val CURRENT_COHORT: String = "aaaa1111"

    /** A hash for a phone the account has seen before, across a reinstall. */
    const val RETURNING_COHORT: String = "bbbb2222"

    /** One session, with every deadline placed relative to the library's "now". */
    fun session(
        id: String,
        cohortHash: String?,
        isCurrent: Boolean = false,
        createdDaysAgo: Long = 30,
        activeHoursAgo: Long = 2,
        deviceId: String? = null,
        userAgent: String? = null,
    ): Session {
        val createdAt = NOW.minus(Duration.ofDays(createdDaysAgo))
        val lastActive = NOW.minus(Duration.ofHours(activeHoursAgo))
        return Session(
            id = SessionId(id),
            userId = OWNER,
            createdAt = createdAt,
            lastActiveAt = lastActive,
            inactivityExpiresAt = lastActive.plus(Session.DEFAULT_INACTIVITY_WINDOW),
            hardExpiresAt = createdAt.plus(Session.DEFAULT_HARD_LIFETIME),
            isCurrent = isCurrent,
            deviceId = deviceId?.let(::DeviceId),
            cohortHash = cohortHash,
            userAgent = userAgent,
        )
    }

    /** One durable cohort row. */
    fun cohort(
        hash: String,
        firstSeenDaysAgo: Long = 400,
        lastSeenHoursAgo: Long = 2,
    ): DeviceCohort =
        DeviceCohort(
            cohortHash = hash,
            firstSeen = NOW.minus(Duration.ofDays(firstSeenDaysAgo)),
            lastSeen = NOW.minus(Duration.ofHours(lastSeenHoursAgo)),
        )

    /** One directory row. */
    fun device(
        id: String,
        name: String?,
        isThisDevice: Boolean = false,
        revokedDaysAgo: Long? = null,
    ): Device =
        Device(
            id = DeviceId(id),
            displayName = name,
            platform = PlatformTag.Android.asWire(),
            addedAt = NOW.minus(Duration.ofDays(DEVICE_ADDED_DAYS_AGO)),
            revokedAt = revokedDaysAgo?.let { NOW.minus(Duration.ofDays(it)) },
            isThisDevice = isThisDevice,
        )

    /** A ledger with this device, a device seen before, and one session asserting no cohort. */
    fun ledger(): List<Session> =
        listOf(
            session(
                id = "session-current",
                cohortHash = CURRENT_COHORT,
                isCurrent = true,
                deviceId = "device-a",
                userAgent = "Capsule/1.0 (Android 16; Pixel 8 Pro)",
            ),
            session(
                id = "session-returning-new",
                cohortHash = RETURNING_COHORT,
                deviceId = "device-b",
                userAgent = "Capsule/1.0 (macOS 15.4; arm64)",
            ),
            session(
                id = "session-returning-old",
                cohortHash = RETURNING_COHORT,
                createdDaysAgo = 300,
                activeHoursAgo = 40,
                deviceId = "device-c",
                userAgent = "Capsule/1.0 (macOS 14.7; arm64)",
            ),
            session(
                id = "session-ungrouped",
                cohortHash = null,
                deviceId = "device-d",
                userAgent = "Capsule/1.0 (iPadOS 18.2)",
            ),
        )

    /** The durable map behind [ledger]. */
    fun cohorts(): List<DeviceCohort> = listOf(cohort(CURRENT_COHORT), cohort(RETURNING_COHORT))

    /** A settled sessions state over [ledger]. */
    fun sessionsState(): SessionsUiState =
        SessionsUiState(
            isLoading = false,
            groups = SessionCohorts.group(ledger(), cohorts()),
            devices =
                listOf(
                    device(id = "device-a", name = "Pixel 8 Pro", isThisDevice = true),
                    device(id = "device-b", name = "MacBook Pro"),
                    device(id = "device-c", name = "Pixel 6", revokedDaysAgo = 20),
                ),
        )

    /** A settled credentials state. */
    fun credentialsState(
        passkeys: List<Passkey> = listOf(passkey("passkey-a")),
        totp: TotpEnrollment? = null,
    ): CredentialsUiState = CredentialsUiState(isLoading = false, passkeys = passkeys, totp = totp)

    /** One registered passkey. */
    fun passkey(
        id: String,
        label: String = "Pixel 8 Pro",
        lastUsedAt: Instant? = null,
    ): Passkey =
        Passkey(
            id = PasskeyId(id),
            label = label,
            addedAt = NOW.minus(Duration.ofDays(PASSKEY_ADDED_DAYS_AGO)),
            lastUsedAt = lastUsedAt,
        )

    private const val DEVICE_ADDED_DAYS_AGO = 420L
    private const val PASSKEY_ADDED_DAYS_AGO = 60L
}
