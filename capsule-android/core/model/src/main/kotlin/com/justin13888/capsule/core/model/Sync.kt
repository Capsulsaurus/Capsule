package com.justin13888.capsule.core.model

import java.time.Duration

/**
 * A rung of the upload ladder, mirroring the download ladder.
 *
 * Under [UploadPolicy.Staged] the tiers open in this order per asset, each gated by its own
 * connection rule. The point is that the *index* of what exists escapes the device first: if
 * the phone drowns, the user knows exactly what was lost and holds a preview of it, even
 * though the originals never left.
 *
 * This is client-side session ordering only. The server has no mode branch — under `staged`
 * the client simply has not opened the T2 session yet.
 */
enum class UploadTier(
    override val wire: String,
    val rank: Int,
) : WireEnum {
    /** Signed manifest + metadata blob (with the embedded LQIP). A few KB per asset. */
    Index("t0", 0),

    /** Thumbnail and preview derivative blobs. */
    Preview("t1", 1),

    /** The original blob. */
    Original("t2", 2),
    ;

    /**
     * The weakest connection class that may open this tier's session.
     *
     * T0 opens on anything usable — its whole purpose is to escape a bad link — while T2 is
     * bulk work and waits for an unmetered one.
     */
    val opensAt: ConnectionClass
        get() =
            when (this) {
                Index -> ConnectionClass.Adverse
                Preview -> ConnectionClass.Metered
                Original -> ConnectionClass.Unmetered
            }

    companion object : WireCodec<UploadTier>({ entries })
}

/** Whether an asset's upload sessions open eagerly or in tier order. */
enum class UploadPolicy(
    override val wire: String,
) : WireEnum {
    /** Every session of the bundle opens eagerly, in any order. The default. */
    Full("full"),

    /** Sessions open strictly T0 → T1 → T2, each gated by the connection rules. */
    Staged("staged"),
    ;

    companion object : WireCodec<UploadPolicy>({ entries })
}

/**
 * The quality of the device's current network path — evaluated continuously on-device.
 *
 * Ordered from best to worst so a gate can be written as a comparison. [Adverse] is the one
 * class no OS reports: it is derived from behaviour — repeated mid-transfer resets, stalls,
 * or black-holes inside a sliding window — and demoted again after a clean period. Without
 * it, a link that is technically "connected" but practically unusable would keep the
 * scheduler retrying forever.
 */
enum class ConnectionClass(
    override val wire: String,
) : WireEnum {
    /** Bulk transfer is acceptable. */
    Unmetered("unmetered"),

    /** A byte-counted link; bulk work is deferred. */
    Metered("metered"),

    /** OS-level data saver is active (iOS Low Data Mode, Android Data Saver). */
    Constrained("constrained"),

    /** Connectivity is present but unreliable. Derived, never reported by the OS. */
    Adverse("adverse"),

    /** No usable path. Never promoted to [Adverse]; offline is offline. */
    Offline("offline"),
    ;

    /** Whether any transfer at all can be attempted. */
    val isUsable: Boolean get() = this != Offline

    /** Whether bulk work — original-tier transfer, large reconciliations — may proceed. */
    val allowsBulk: Boolean get() = this == Unmetered

    companion object : WireCodec<ConnectionClass>({ entries })
}

/**
 * How much work a sync check found waiting, which decides how patient the scheduler is.
 *
 * The threshold is not a byte count in this type on purpose — it scales with total upload
 * plus download and is a platform tuning decision — but the *consequence* is a contract:
 * small reconciliations go on any non-metered class, large ones wait for unmetered Wi-Fi.
 */
enum class ReconciliationSize(
    override val wire: String,
) : WireEnum {
    /** A handful of new assets, or metadata-only deltas. */
    Small("small"),

    /** Bulk uploads, or original-tier downloads. A streaming import is always this. */
    Large("large"),
    ;

    /**
     * Whether this much work may proceed on [connection].
     *
     * A user-initiated force-sync bypasses this with explicit consent; background work never
     * does.
     */
    fun mayProceedOn(connection: ConnectionClass): Boolean =
        when (this) {
            Small -> connection == ConnectionClass.Unmetered || connection == ConnectionClass.Metered
            Large -> connection.allowsBulk
        }

    companion object : WireCodec<ReconciliationSize>({ entries })
}

/**
 * Which retry shape a surface instantiates.
 *
 * Every retrying surface declares one. Universal rules apply to all three: exponential
 * backoff with full jitter, server backoff signals honoured, and a bounded give-up that
 * surfaces a user-visible state — no surface ever hot-loops.
 */
enum class RetryClass(
    override val wire: String,
) : WireEnum {
    /** Auth flows and on-demand tier fetches: short, then a visible failure. */
    Interactive("interactive"),

    /** Upload and download resume: resume-first, patient within the session's lifetime. */
    BulkTransfer("bulk-transfer"),

    /** MLS recovery, federation circuit breaker: a slow ladder that never abandons silently. */
    ControlCeremony("control-ceremony"),
    ;

    /** The bounds this class retries within. */
    val policy: RetryPolicy
        get() =
            when (this) {
                Interactive -> RetryPolicy(2, Duration.ofMillis(200), Duration.ofSeconds(2))
                BulkTransfer -> RetryPolicy(8, Duration.ofMillis(500), Duration.ofSeconds(30))
                ControlCeremony -> RetryPolicy(12, Duration.ofSeconds(30), Duration.ofSeconds(600))
            }

    companion object : WireCodec<RetryClass>({ entries })
}

/**
 * The bounds of one [RetryClass].
 *
 * [maxRetries] is what turns a retry loop into a bounded one; without it a surface can keep
 * a device awake indefinitely on a link that is never coming back.
 */
data class RetryPolicy(
    /** Attempts after the first failure before a visible give-up. */
    val maxRetries: Int,
    /** The first backoff ceiling; doubles each attempt up to [maxDelay]. */
    val baseDelay: Duration,
    /** The cap on the backoff ceiling. */
    val maxDelay: Duration,
) {
    /**
     * The backoff ceiling for [attempt] (zero-based), doubling from [baseDelay] and capped at
     * [maxDelay]. The actual wait is a jittered draw below this ceiling.
     */
    fun ceilingFor(attempt: Int): Duration {
        if (attempt >= maxRetries) return maxDelay
        val doubled = baseDelay.multipliedBy(1L shl attempt.coerceAtMost(MAX_SHIFT))
        return if (doubled > maxDelay) maxDelay else doubled
    }

    /** Whether another attempt is permitted after [attempt] (zero-based) failed. */
    fun mayRetry(attempt: Int): Boolean = attempt < maxRetries

    private companion object {
        /** Keeps the shift inside `Long`'s range regardless of how high an attempt count climbs. */
        const val MAX_SHIFT = 30
    }
}

/**
 * Where an asset sits relative to the server.
 *
 * [AwaitingOriginal] is the state worth having a name for. It is **derived** from the sync
 * feed's `original_held` flag and never stored as a second source of truth, and it is what a
 * client shows instead of a failure: fetching the original of such an asset returns a
 * transient pending code, explicitly distinct from a permanent `410 Gone`, so the UI shows an
 * "original still on *device*" badge and re-fetches when the feed flips.
 */
sealed interface SyncState {
    /** Everything this library's scope calls for is on the server and locally reconciled. */
    data object Synced : SyncState

    /** Never uploaded — the library has no server, or the device has never been online. */
    data object LocalOnly : SyncState

    /** A tier's session is queued but not yet open. */
    data class Queued(
        val nextTier: UploadTier,
    ) : SyncState

    /** A tier's session is open and transferring. */
    data class Uploading(
        /** Which rung is in flight. */
        val tier: UploadTier,
        /** Bytes confirmed by the server so far. */
        val bytesSent: Long,
        /** The session's declared size. */
        val bytesTotal: Long,
    ) : SyncState

    /**
     * Visible everywhere, but the original has not landed yet.
     *
     * Its local original is pinned until the original's session completes and durability is
     * confirmed — no release path may fire before then.
     */
    data object AwaitingOriginal : SyncState

    /** Inbound state was refused and is waiting for a human decision. */
    data class Quarantined(
        val surface: QuarantineSurface,
    ) : SyncState

    /** The last attempt failed with a code the user can act on. */
    data class Failed(
        val code: ErrorCode,
    ) : SyncState
}
