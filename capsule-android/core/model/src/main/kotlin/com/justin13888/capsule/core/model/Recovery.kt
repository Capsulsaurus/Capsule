package com.justin13888.capsule.core.model

import java.time.Duration
import java.time.Instant

/**
 * How the user can get back to their account master key.
 *
 * Every path terminates at the one master key, and the recovery secret (or a quorum of its
 * shares) is the only user-held secret class. There is deliberately no second escrowed
 * secret and no path that bypasses the master key — a second root would double the number of
 * things that can be lost while halving the attention paid to each.
 */
enum class RecoveryMechanism(
    override val wire: String,
) : WireEnum {
    /** A recovery passphrase or seed phrase the user holds. */
    Passphrase("passphrase"),

    /** An already-enrolled device vouching for a new one. */
    CrossDevice("cross_device"),

    /** Opt-in Shamir shares, default 2-of-3, each separately wrapped. */
    SharedSecret("shared_secret"),
    ;

    companion object : WireCodec<RecoveryMechanism>({ entries })
}

/** The result of checking an entered recovery secret against the escrow blob. */
enum class RecoveryVerifyOutcome(
    override val wire: String,
) : WireEnum {
    /** The secret unwraps the escrow to the device-held master key. */
    Verified("verified"),

    /** It does not — a wrong secret, or a cached blob gone stale after a rotation. */
    NotVerified("not_verified"),
    ;

    companion object : WireCodec<RecoveryVerifyOutcome>({ entries })
}

/**
 * Where the user stands on proving they can still recover their account.
 *
 * A recovery secret nobody has ever re-entered is a recovery secret nobody knows they have
 * lost, so the client asks periodically — on a backing-off cadence, never blocking anything.
 * This type is the client-side derivation of that cadence; it is not synced state.
 */
sealed interface RecoveryState {
    /** No recovery secret has been set up. */
    data object NotConfigured : RecoveryState

    /** Set up and last verified successfully; the next prompt is scheduled. */
    data class Healthy(
        /** When the last successful verification happened. */
        val lastVerifiedAt: Instant,
        /** When the next prompt is due. */
        val nextPromptAt: Instant,
        /** How many verifications have succeeded consecutively, which widens the interval. */
        val consecutiveSuccesses: Int,
    ) : RecoveryState

    /** The prompt is due and has not been answered. */
    data class VerificationDue(
        val since: Instant,
    ) : RecoveryState

    /** The user snoozed the prompt. Snoozing suppresses the ask, never the requirement. */
    data class Snoozed(
        /** When the prompt returns. */
        val until: Instant,
        /** How many times in a row it has been snoozed. */
        val consecutiveSnoozes: Int,
    ) : RecoveryState

    /**
     * Verification has failed enough times that the user probably no longer holds the secret.
     *
     * The response is a guided re-wrap, not a lockout: the account is still reachable from
     * this device, and the point is to establish a secret the user *does* hold before that
     * stops being true.
     */
    data class RewrapRecommended(
        val consecutiveFailures: Int,
    ) : RecoveryState
}

/**
 * The backing-off cadence on which the client asks the user to prove they can recover.
 *
 * Short at first, then widening: the first weeks after setup are when a written-down secret
 * is most likely to be lost or mistranscribed, and once the user has proved it twice the ask
 * becomes an annoyance rather than a service.
 */
object RecoveryCadence {
    /** The first prompt, after setup or after any re-arming event. */
    val FIRST_INTERVAL: Duration = Duration.ofDays(7)

    /** The interval after one success. */
    val SECOND_INTERVAL: Duration = Duration.ofDays(90)

    /** The ceiling the interval backs off to. */
    val MAX_INTERVAL: Duration = Duration.ofDays(180)

    /** The most consecutive snoozes before the prompt becomes a persistent, non-blocking badge. */
    const val MAX_CONSECUTIVE_SNOOZES = 3

    /** Failures across at least two app sessions that trigger a guided re-wrap. */
    const val FAILURES_BEFORE_REWRAP = 3

    /**
     * The interval to wait after [consecutiveSuccesses] successful verifications.
     *
     * Zero or negative counts are treated as none, so a caller that has not tracked history
     * gets the conservative short interval rather than the ceiling.
     */
    fun intervalAfter(consecutiveSuccesses: Int): Duration =
        when {
            consecutiveSuccesses <= 0 -> FIRST_INTERVAL
            consecutiveSuccesses == 1 -> SECOND_INTERVAL
            else -> MAX_INTERVAL
        }
}

/**
 * What a restore may do to the live library.
 *
 * The default is [DryRun], and that is a safety property rather than a convenience: a
 * restore that silently overwrote current state would be indistinguishable from the
 * stale-revival attack the provenance chain exists to prevent. [Commit] requires an explicit
 * act after the dry-run report has been shown.
 */
enum class RestoreMode(
    override val wire: String,
) : WireEnum {
    /** Verify structure only. No decryption, no writes. */
    Preview("preview"),

    /** Verify decryption and hashes, compute the diff, write nothing. The default. */
    DryRun("dry_run"),

    /** Apply the computed diff. Never a silent overwrite. */
    Commit("commit"),
    ;

    /** Whether this mode may change the live library. */
    val writes: Boolean get() = this == Commit

    companion object : WireCodec<RestoreMode>({ entries })
}

/**
 * How one asset in a backup compares to the live library.
 *
 * The four cases are the reconciliation ladder, decided by comparing the restored manifest
 * against the live chain head. Only [AbsentLocally] is applied automatically; the middle two
 * are surfaced and left alone, because "the backup is older" is never on its own a reason to
 * roll a user's library back.
 *
 * The wire tokens are minted here — the design states the ladder in prose without assigning
 * strings — so they are this client's vocabulary rather than a cross-implementation contract.
 */
enum class RestoreConflictKind(
    override val wire: String,
) : WireEnum {
    /** The heads match. A no-op, which is what makes restore idempotent. */
    Identical("identical"),

    /** The live head chains forward from the restored one; the live copy is newer. */
    LiveHeadNewer("live_head_newer"),

    /** The chains diverge, or the asset is locally tombstoned at a later step. */
    Divergent("divergent"),

    /** The asset is not in the live library at all. Applied directly on commit. */
    AbsentLocally("absent_locally"),
    ;

    /** Whether a commit applies this case without asking. */
    val appliesOnCommit: Boolean get() = this == AbsentLocally

    /** Whether the case is surfaced for an explicit human decision. */
    val needsReview: Boolean get() = this == LiveHeadNewer || this == Divergent

    companion object : WireCodec<RestoreConflictKind>({ entries })
}

/** One asset's position on the reconciliation ladder. */
data class RestoreConflict(
    /** The asset in question. */
    val assetId: AssetId,
    /** How the backup's chain head compares to the live one. */
    val kind: Wire<RestoreConflictKind>,
    /** The chain head the backup carries. */
    val backupHead: String?,
    /** The chain head the live library holds. */
    val liveHead: String?,
)

/**
 * What a backup artifact contains, without opening it.
 *
 * There is no aggregate size field on the artifact itself — sizes are per entry — so
 * [totalBytes] here is the sum this client computed while reading the manifest, and is
 * labelled as such rather than presented as something the artifact asserts.
 *
 * Both authentications must pass before any of this is trusted: a keyed MAC over the
 * manifest, and a hybrid signature from the exporting device. An exporter that was later
 * revoked is rejected — the device directory keeps its public half forever precisely so that
 * check can still be made.
 */
data class BackupArtifactSummary(
    /** This client's handle for the artifact. */
    val id: BackupArtifactId,
    /** The artifact container format version. */
    val artifactVersion: Int,
    /** The cryptographic suite the contents were sealed under. */
    val cryptoSuiteId: Int,
    /** The lowest protocol version a client must speak to restore it. */
    val minProtocolVersion: String,
    /** The device that exported it. Must still be in the directory at restore time. */
    val exporterDevice: DeviceId,
    /** When the export ran. */
    val exportedAt: Instant,
    /** How many entries the manifest lists. */
    val entryCount: Int,
    /** The sum of the manifest's per-entry sizes, computed by this client. */
    val totalBytes: Long,
    /** Whether the manifest MAC and the exporter signature both verified. */
    val isAuthenticated: Boolean,
)

/**
 * What a restore would do, or did.
 *
 * The same shape in every [RestoreMode], which is the point: the dry run the user approves
 * and the commit that follows report identically, so what they confirmed is what happened.
 * [applied] is populated only by a commit.
 */
data class RestoreReport(
    /** Entries whose content hash — and, past a preview, whose decryption — verified. */
    val verified: Int,
    /** Assets not present locally. Applied directly on commit. */
    val wouldAdd: List<AssetId>,
    /** Assets already at the same chain head. No-ops, which is what makes restore idempotent. */
    val identical: List<AssetId>,
    /** Assets whose local head differs. Not applied; surfaced for an explicit merge. */
    val conflicts: List<RestoreConflict>,
    /** Assets actually written. Empty unless the mode was [RestoreMode.Commit]. */
    val applied: List<AssetId> = emptyList(),
) {
    /** Whether anything needs a human decision before this artifact is fully absorbed. */
    val needsReview: Boolean get() = conflicts.any { it.kind.knownOrNull?.needsReview == true }
}
