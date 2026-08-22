package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Refusals that come from an operator decision rather than from the user's own state.
 *
 * Surfaced to the affected user rather than swallowed. Someone whose account was suspended needs
 * to be told that, not left watching uploads fail for no stated reason.
 */
sealed class ModerationError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 403 at POST /upload session creation (slice S-C8): the account carries an admin/billing
     * suspension flag.
     */
    data object AccountSuspended : ModerationError(
        code = "error.moderation.account_suspended",
        detail = "Your account is suspended. You can't upload or share until it's reinstated.",
    )

    /**
     * HTTP 429 on federated report intake (slice S-C8, threat-model invariant 24): the
     * per-(reporting_server, reported_user) report budget for the current window is exhausted.
     */
    data object ReportRateLimited : ModerationError(
        code = "error.moderation.report_rate_limited",
        detail = "Too many reports from this source. Please wait and try again.",
    )

    /**
     * Federated report intake (slice S-C8, threat-model invariant 24): the report's Ed25519
     * signature did not verify against a known peer key — unsigned, tampered, or from an
     * unregistered peer — or the report was structurally malformed.
     */
    data object ReportUnsigned : ModerationError(
        code = "error.moderation.report_unsigned",
        detail = "The moderation report could not be verified.",
    )

    /**
     * HTTP 403 (federation pull / report intake, slice S-C8): the requesting peer server is on this
     * server's blocklist, so its federated request — a pull or a moderation report — is refused at
     * the federation-capability layer.
     */
    data object ServerBlocked : ModerationError(
        code = "error.moderation.server_blocked",
        detail = "This server is blocked from federating with us.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<ModerationError> =
            listOf(
                AccountSuspended,
                ReportRateLimited,
                ReportUnsigned,
                ServerBlocked,
            )
    }
}
