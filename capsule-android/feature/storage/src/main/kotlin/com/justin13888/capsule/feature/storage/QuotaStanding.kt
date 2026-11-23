package com.justin13888.capsule.feature.storage

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.QuotaState
import com.justin13888.capsule.core.model.QuotaStatus
import java.time.Duration
import java.time.Instant

/**
 * The one thing the user should do about their current quota state.
 *
 * A separate axis from [QuotaState] rather than a `when` inside the screen, because the
 * mapping is the interesting decision and it is not one-to-one with severity: the two states
 * that refuse uploads want the *same* remediation phrased differently, and the suspended
 * state — the most severe — is the only one the user cannot act on at all.
 */
enum class QuotaRemediation {
    /** Below the soft limit. Nothing is owed.  */
    None,

    /** Approaching the wall. Looking at what is large now avoids an interruption later. */
    ReviewLargest,

    /** Past the hard limit. Freeing space is what makes uploads resume. */
    FreeSpace,

    /**
     * Past the grace window too.
     *
     * Deliberately distinct from [FreeSpace]: deletes and restore-from-trash are still
     * admitted here by design, so the screen has to foreground freeing space rather than
     * present a dead end. A user must be able to delete their way back under quota.
     */
    FreeSpaceToResumeEdits,

    /** An administrative action. Only an operator can lift it, so the user is pointed at one. */
    ContactSupport,
}

/**
 * The account's standing against its limits, in the shape a screen draws.
 *
 * Derived from [QuotaStatus] rather than stored beside it so the numbers, the state, and the
 * advice can never disagree — the same reason the domain derives [QuotaState] from usage
 * instead of carrying it as a field.
 */
@Immutable
data class QuotaStanding(
    /** Where the account sits on the ladder. */
    val state: QuotaState,
    /** Bytes attributed to this uploader. */
    val used: Long,
    /** Where the warning starts. */
    val softLimit: Long,
    /** Where uploads stop, or `null` on a deployment running without quota. */
    val hardLimit: Long?,
    /**
     * How long the grace window has left, or `null` when it does not apply.
     *
     * Only meaningful in [QuotaState.HardExceeded]: before the hard limit there is no window
     * running, and after it expires there is nothing left to count down.
     */
    val graceRemaining: Duration?,
) {
    /** How full the account is, `0f..1f`, against the hard limit where there is one. */
    val fraction: Float =
        when (val ceiling = hardLimit ?: softLimit) {
            0L -> 0f
            else -> (used.toDouble() / ceiling.toDouble()).coerceIn(0.0, 1.0).toFloat()
        }

    /** What the screen should offer to do about it. */
    val remediation: QuotaRemediation =
        when (state) {
            QuotaState.Ok -> QuotaRemediation.None
            QuotaState.SoftWarning -> QuotaRemediation.ReviewLargest
            QuotaState.HardExceeded -> QuotaRemediation.FreeSpace
            QuotaState.GraceExpired -> QuotaRemediation.FreeSpaceToResumeEdits
            QuotaState.Suspended -> QuotaRemediation.ContactSupport
        }

    /** Whether a new upload session may be created. */
    val allowsUpload: Boolean get() = state.allowsUpload

    /** Whether a caption, a tag, or a new link is admitted. */
    val allowsMetadataGrowth: Boolean get() = state.allowsMetadataGrowth

    /**
     * Whether deletes, trash-empties and restores still go through.
     *
     * Surfaced on every state card, not only the ones where it is in doubt: the states where
     * a user most needs to know that deleting still works are exactly the states that look
     * most like a lockout.
     */
    val allowsRecoveryWrites: Boolean get() = state.allowsRecoveryWrites

    companion object {
        /** Reads [status] at [now]. */
        fun of(
            status: QuotaStatus,
            now: Instant,
        ): QuotaStanding {
            val state = status.stateAt(now)
            return QuotaStanding(
                state = state,
                used = status.used,
                softLimit = status.softLimit,
                hardLimit = status.hardLimit,
                graceRemaining = status.graceRemainingAt(now).takeIf { state == QuotaState.HardExceeded },
            )
        }

        /**
         * What is left of the grace window at [now].
         *
         * Clamped at zero rather than allowed to go negative: a window that has run out is
         * the [QuotaState.GraceExpired] state, and a negative duration would render as a
         * countdown running backwards.
         */
        private fun QuotaStatus.graceRemainingAt(now: Instant): Duration? {
            val crossed = hardLimitCrossedAt ?: return null
            val remaining = Duration.between(now, crossed.plus(graceWindow))
            return if (remaining.isNegative) Duration.ZERO else remaining
        }
    }
}
