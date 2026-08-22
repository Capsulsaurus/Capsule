package com.justin13888.capsule.core.model

import java.time.Duration
import java.time.Instant

/**
 * An account's standing against its storage limits.
 *
 * The ladder is ordered by severity, and every step past [Ok] narrows what the server will
 * accept rather than what the user can see: existing assets stay readable in every state,
 * including [Suspended]. That asymmetry is deliberate — quota is a storage-growth control,
 * and locking a user out of their own photos would be a data-availability failure dressed up
 * as billing enforcement.
 */
enum class QuotaState(
    override val wire: String,
    val severity: Int,
) : WireEnum {
    /** Below the soft limit. Everything succeeds. */
    Ok("ok", 0),

    /** At or past the soft limit. Uploads still succeed; the UI warns. */
    SoftWarning("soft_warning", 1),

    /**
     * At or past the hard limit. New upload sessions are refused; metadata edits and every
     * other write still work.
     */
    HardExceeded("hard_exceeded", 2),

    /**
     * Past the hard limit for longer than the grace window.
     *
     * Metadata-growth writes are refused too — but reads, deletes, and restore-from-trash
     * keep working, and the provenance a delete or a trash-empty itself produces is **always**
     * admitted. A user must be able to delete their way back under quota; a state that
     * blocked deletion would be a trap with no exit.
     */
    GraceExpired("grace_expired", 3),

    /** An administrative or billing action. Server-defined; not derived from usage. */
    Suspended("suspended", 4),
    ;

    /** Whether a new upload session may be created. */
    val allowsUpload: Boolean get() = severity < HardExceeded.severity

    /** Whether an ordinary metadata-growth write — a caption, a tag, a new link — is admitted. */
    val allowsMetadataGrowth: Boolean get() = severity < GraceExpired.severity

    /**
     * Whether the writes a delete, trash-empty, or trash-restore produces are admitted.
     *
     * True in every derived state. Only an administrative [Suspended] can close this door,
     * and even then the user retains the master-key-gated ability to secure their account.
     */
    val allowsRecoveryWrites: Boolean get() = this != Suspended

    companion object : WireCodec<QuotaState>({ entries })
}

/**
 * What the server currently accounts to this uploader.
 *
 * Usage is attributed to the authenticated uploader rather than the asset's owner, so a user
 * uploading on someone else's behalf pays for the bytes they caused. Trash counts at full
 * size until its retention window expires — which is the point: it keeps the accounting
 * honest and gives the user a concrete reason to empty trash rather than treat it as free
 * overflow.
 */
data class QuotaStatus(
    /** Bytes attributed to this uploader. */
    val used: Long,
    /** Where the warning starts. */
    val softLimit: Long,
    /**
     * Where uploads stop.
     *
     * `null` means no limit — a self-hosted deployment running without quota. The absence is
     * modelled rather than encoded as a sentinel so an unlimited server cannot be mistaken
     * for one that is astronomically full.
     */
    val hardLimit: Long?,
    /** How long past the hard limit before the state escalates. */
    val graceWindow: Duration = DEFAULT_GRACE_WINDOW,
    /** When usage first crossed the hard limit, if it has. */
    val hardLimitCrossedAt: Instant? = null,
    /** Set by an administrative action, independent of usage. */
    val isSuspended: Boolean = false,
) {
    /** Bytes still available, or `null` on an unlimited deployment. */
    val remaining: Long? get() = hardLimit?.let { (it - used).coerceAtLeast(0) }

    /**
     * Derives the state at [now].
     *
     * Derived rather than stored so it can never disagree with the numbers it summarises —
     * the same reason a stack's cull state is derived from its members.
     */
    fun stateAt(now: Instant): QuotaState {
        if (isSuspended) return QuotaState.Suspended
        val hard = hardLimit ?: return if (used >= softLimit) QuotaState.SoftWarning else QuotaState.Ok
        return when {
            used < softLimit -> QuotaState.Ok
            used < hard -> QuotaState.SoftWarning
            isGraceExpired(now) -> QuotaState.GraceExpired
            else -> QuotaState.HardExceeded
        }
    }

    /**
     * Whether [additionalBytes] may be reserved at [now].
     *
     * Mirrors the single hard enforcement point: the check runs at session creation against
     * the declared size, and once a session is open the declared size is the cap.
     */
    fun admitsUpload(
        additionalBytes: Long,
        now: Instant,
    ): Boolean {
        if (!stateAt(now).allowsUpload) return false
        val hard = hardLimit ?: return true
        return used + additionalBytes <= hard
    }

    private fun isGraceExpired(now: Instant): Boolean {
        val crossed = hardLimitCrossedAt ?: return false
        return !now.isBefore(crossed.plus(graceWindow))
    }

    companion object {
        /** The default grace window before the hard-exceeded state escalates. */
        val DEFAULT_GRACE_WINDOW: Duration = Duration.ofDays(14)
    }
}
