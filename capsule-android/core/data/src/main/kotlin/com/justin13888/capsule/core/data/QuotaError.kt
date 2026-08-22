package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Refusals driven by the uploader's storage accounting.
 *
 * Separate codes for the hard limit and the grace-expired state, because they call for different
 * advice: the first says "free space to upload", the second says "edits are paused too".
 */
sealed class QuotaError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 403 at upload-session creation when the declared size would cross the uploader's hard
     * quota limit.
     */
    data object Exceeded : QuotaError(
        code = "error.quota.exceeded",
        detail = "Your storage is full.",
    )

    /**
     * HTTP 403 in the Grace-expired quota state: quota_used has stayed at/above the hard limit past
     * the grace window, so metadata-growth writes (caption/tag edits, new share or upload links) are
     * refused.
     */
    data object GraceLocked : QuotaError(
        code = "error.quota.grace_locked",
        detail = "Your storage has been full past the grace period, so edits are paused until you free space.",
    )

    /**
     * HTTP 403 when a federated blob cached on a user's behalf would cross the per-(receiving_user,
     * source_peer) caching budget (default 25% of the receiver's hard quota per source peer).
     */
    data object PeerBudgetExceeded : QuotaError(
        code = "error.quota.peer_budget_exceeded",
        detail = "This source has reached its caching limit on your account.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<QuotaError> =
            listOf(
                Exceeded,
                GraceLocked,
                PeerBudgetExceeded,
            )
    }
}
