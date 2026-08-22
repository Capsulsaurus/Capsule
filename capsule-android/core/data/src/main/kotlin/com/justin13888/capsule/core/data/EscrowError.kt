package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures reading the recovery escrow blob.
 *
 * Reading the escrow is local and keyless, so a malformed blob is a repair problem rather than a
 * credential problem, and is surfaced as such.
 */
sealed class EscrowError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 400 on master-key escrow store (slice S-C12): the request body failed the coarse size
     * sanity bound (empty, or larger than any legitimate wrapped master key).
     */
    data object Malformed : EscrowError(
        code = "error.escrow.malformed",
        detail = "The recovery backup could not be saved.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<EscrowError> =
            listOf(
                Malformed,
            )
    }
}
