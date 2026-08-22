package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures on the durability-verification surface.
 *
 * The verify-before-destroy gate reports here. A failure means the client must **not** release its
 * local copy, which is why it is an error rather than an ignorable signal.
 */
sealed class StorageError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 429 from POST /storage/verify with deep=true (slice S-C3): the caller exceeded its per-
     * user deep re-hash budget for the current window.
     */
    data object DeepRateLimited : StorageError(
        code = "error.storage.deep_rate_limited",
        detail = "Too many deep storage checks. Please wait and try again.",
    )

    /**
     * HTTP 400 from POST /storage/verify (slice S-C3): a declared blob hash is not 64-char lowercase
     * hex, or an asset_id is empty.
     */
    data object InvalidRequest : StorageError(
        code = "error.storage.invalid_request",
        detail = "The storage-verification request was malformed.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<StorageError> =
            listOf(
                DeepRateLimited,
                InvalidRequest,
            )
    }
}
