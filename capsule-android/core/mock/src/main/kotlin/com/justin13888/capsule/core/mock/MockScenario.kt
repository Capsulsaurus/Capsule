package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.QuotaState
import java.time.Duration

/** How long each operation takes before it answers. */
data class LatencyProfile(
    /** The delay applied to any operation without an entry in [perOperation]. */
    val default: Duration = Duration.ZERO,
    /** Per-operation overrides, for making one screen slow without slowing the rest. */
    val perOperation: Map<MockOperation, Duration> = emptyMap(),
) {
    /** The delay [operation] should take. */
    fun forOperation(operation: MockOperation): Duration = perOperation[operation] ?: default

    companion object {
        /** No delay at all — the default, and what a screenshot test wants. */
        val INSTANT = LatencyProfile()

        /** A healthy connection: fast enough to feel local, slow enough to show a spinner. */
        val REALISTIC =
            LatencyProfile(
                default = Duration.ofMillis(40),
                perOperation =
                    mapOf(
                        MockOperation.RepresentationFetch to Duration.ofMillis(220),
                        MockOperation.SyncControl to Duration.ofMillis(600),
                        MockOperation.ImportRun to Duration.ofMillis(400),
                    ),
            )

        /** A bad link — long enough that every loading and timeout state is reachable by hand. */
        val SLOW = LatencyProfile(default = Duration.ofMillis(2_500))
    }
}

/** What the server currently accounts to the uploader. */
enum class QuotaScenario(
    /** The state a repository reports and enforces against. */
    val state: QuotaState,
) {
    /** Below the soft limit. */
    Ok(QuotaState.Ok),

    /** At or past the soft limit; uploads still succeed and the UI warns. */
    SoftWarning(QuotaState.SoftWarning),

    /** Past the hard limit; new upload sessions are refused, metadata edits still work. */
    HardExceeded(QuotaState.HardExceeded),

    /** Past the hard limit beyond the grace window; metadata growth is refused too. */
    GraceExpired(QuotaState.GraceExpired),

    /** An administrative action, independent of usage. */
    Suspended(QuotaState.Suspended),
}

/** What happens when the original tier is asked for. */
enum class OriginalFetchScenario {
    /** It is there. */
    Available,

    /** It has not left the capturing device yet — a transient pending, not a loss. */
    AwaitingUpload,

    /** `410 Gone`. Degrades to the best held rung; the asset stays listed. */
    Gone,

    /** `403`. The viewer's access changed, so membership is re-synced before degrading. */
    MembershipChanged,
}

/** What a sync does with the state a peer proposes. */
enum class InboundStateScenario {
    /** It applies cleanly. */
    Applied,

    /** It names a chain head this device has moved past, so it is quarantined. */
    StaleRevival,
}

/** How adding a device to the account ends. */
enum class EnrollmentScenario {
    /** The safety codes match and the device joins. */
    Succeeds,

    /**
     * The safety codes differ — a machine-in-the-middle on the enrollment channel.
     *
     * The whole reason the codes are compared out of band, and the one enrollment failure a
     * user must never be allowed to dismiss past.
     */
    SafetyCodeMismatch,

    /** The two ends speak different protocol versions; the app has to be updated. */
    ProtocolMismatch,
}

/**
 * Every condition the mock backend can be put into, as one value.
 *
 * A single value type rather than a bag of setters, because scenarios are things that get
 * *named*: a screenshot test says `MockScenario.QuotaExhausted` and a debug drawer lists the
 * presets, and both break the moment "the current scenario" is a mutable pile of flags that
 * two callers can half-apply. Copying a preset with one field changed covers everything the
 * setters would have.
 *
 * The presets are the documented states of the app. Each one is reachable in a single line and
 * each one has an outcome a test asserts, which is what keeps the fixture honest as the states
 * multiply.
 */
data class MockScenario(
    /** A stable identifier, used to name a screenshot golden and to label a debug drawer row. */
    val id: String,
    /** The device's current path quality. */
    val connection: ConnectionClass = ConnectionClass.Unmetered,
    /** Whether there is an authenticated session at all. */
    val signedIn: Boolean = true,
    /** Whether the library has any content — `false` is the first-run state. */
    val populated: Boolean = true,
    /** The account's standing against its storage limits. */
    val quota: QuotaScenario = QuotaScenario.Ok,
    /** What an original-tier fetch does. */
    val originalFetch: OriginalFetchScenario = OriginalFetchScenario.Available,
    /** Whether the media sandbox can decode this library's assets. */
    val decodeSucceeds: Boolean = true,
    /** What a sync does with inbound state. */
    val inboundState: InboundStateScenario = InboundStateScenario.Applied,
    /** How adding a device ends. */
    val enrollment: EnrollmentScenario = EnrollmentScenario.Succeeds,
    /** Whether the server still speaks this build's protocol version. */
    val protocolSupported: Boolean = true,
    /** How long ago a reconciliation last completed; `null` means none ever has. */
    val lastSyncAgo: Duration? = Duration.ofMinutes(4),
    /** How long each operation takes. */
    val latency: LatencyProfile = LatencyProfile.INSTANT,
    /**
     * Failures forced on specific operations, whatever the rest of the scenario says.
     *
     * The escape hatch for the long tail: every `error.*` code in the catalogue can be put in
     * front of the UI from a test without a preset having to exist for it.
     */
    val failures: Map<MockOperation, CapsuleError> = emptyMap(),
) {
    /** This scenario with [error] forced on [operation]. */
    fun failing(
        operation: MockOperation,
        error: CapsuleError,
    ): MockScenario = copy(failures = failures + (operation to error))

    @Suppress("TooManyFunctions")
    companion object {
        /** A healthy, populated, signed-in library. */
        val Default = MockScenario(id = "default")

        /** The same library with realistic delays, for driving it by hand. */
        val Realistic = Default.copy(id = "realistic", latency = LatencyProfile.REALISTIC)

        /** No usable network path. Reads keep working; anything remote does not. */
        val Offline = MockScenario(id = "offline", connection = ConnectionClass.Offline)

        /** Signed out — the state the app opens in before onboarding. */
        val NeverSignedIn =
            MockScenario(id = "never-signed-in", signedIn = false, populated = false, lastSyncAgo = null)

        /** Signed in, nothing imported yet. */
        val FirstRun = MockScenario(id = "first-run", populated = false, lastSyncAgo = null)

        /** At the soft limit: the warning is showing and uploads still succeed. */
        val QuotaSoftWarning = MockScenario(id = "quota-soft-warning", quota = QuotaScenario.SoftWarning)

        /** Past the hard limit: new upload sessions are refused. */
        val QuotaExhausted = MockScenario(id = "quota-exhausted", quota = QuotaScenario.HardExceeded)

        /** Past the grace window too: metadata growth is refused, deletes still work. */
        val QuotaGraceExpired = MockScenario(id = "quota-grace-expired", quota = QuotaScenario.GraceExpired)

        /** An administrative suspension. Reads survive; uploads and new links do not. */
        val AccountSuspended = MockScenario(id = "account-suspended", quota = QuotaScenario.Suspended)

        /** The original is still on the capturing device — pending, explicitly not a loss. */
        val AwaitingOriginal =
            MockScenario(id = "awaiting-original", originalFetch = OriginalFetchScenario.AwaitingUpload)

        /** `410 Gone`: the fetch degrades to the best held rung rather than failing. */
        val GoneDegrade = MockScenario(id = "gone-degrade", originalFetch = OriginalFetchScenario.Gone)

        /** `403`: the viewer's album access changed and membership is re-synced first. */
        val MembershipResync =
            MockScenario(id = "membership-resync", originalFetch = OriginalFetchScenario.MembershipChanged)

        /** A peer replayed an old-but-validly-signed manifest, so the write is quarantined. */
        val StaleRevival = MockScenario(id = "stale-revival", inboundState = InboundStateScenario.StaleRevival)

        /** The media sandbox refuses to decode. The placeholder is all there is to draw. */
        val SandboxDecodeFailure = MockScenario(id = "sandbox-decode-failure", decodeSucceeds = false)

        /**
         * An asset written by a newer `protocol_version`.
         *
         * Reachable without any toggle — the fixture always contains one — so the preset exists
         * to *name* the state for a screenshot or a debug drawer rather than to create it.
         */
        val ForwardSchema = MockScenario(id = "forward-schema")

        /** The safety codes did not match while adding a device. */
        val EnrollmentMitm =
            MockScenario(id = "enrollment-mitm", enrollment = EnrollmentScenario.SafetyCodeMismatch)

        /** The server no longer speaks this build's protocol version. */
        val ProtocolMismatch =
            MockScenario(
                id = "protocol-mismatch",
                protocolSupported = false,
                enrollment = EnrollmentScenario.ProtocolMismatch,
            )

        /** Nothing has synced for a fortnight — past the stale-sync warning threshold. */
        val StaleSync = MockScenario(id = "stale-sync", lastSyncAgo = Duration.ofDays(15))

        /** A link that is technically connected but practically unusable, and falling behind. */
        val SyncDegraded =
            MockScenario(
                id = "sync-degraded",
                connection = ConnectionClass.Adverse,
                lastSyncAgo = Duration.ofDays(6),
                latency = LatencyProfile.SLOW,
            )

        /** Every preset, so a debug drawer or a screenshot suite can enumerate them. */
        val ALL: List<MockScenario> =
            listOf(
                Default,
                Realistic,
                Offline,
                NeverSignedIn,
                FirstRun,
                QuotaSoftWarning,
                QuotaExhausted,
                QuotaGraceExpired,
                AccountSuspended,
                AwaitingOriginal,
                GoneDegrade,
                MembershipResync,
                StaleRevival,
                SandboxDecodeFailure,
                ForwardSchema,
                EnrollmentMitm,
                ProtocolMismatch,
                StaleSync,
                SyncDegraded,
            )
    }
}
