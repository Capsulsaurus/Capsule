package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures issuing or serving a share link.
 *
 * A link is served to strangers, so its refusals are rate limits rather than descriptions.
 */
sealed class ShareError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 429 on the public share serve path (slice S-C4): the per-source-IP or per-{opaque-id}
     * serve rate limit engaged.
     */
    data object RateLimited : ShareError(
        code = "error.share.rate_limited",
        detail = "Too many attempts. Please wait and try again.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<ShareError> =
            listOf(
                RateLimited,
            )
    }
}
