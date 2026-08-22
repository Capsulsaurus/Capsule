package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures fetching a content-addressed blob.
 *
 * The single variant here is the one that is **not** a durability loss, which is exactly why it
 * needs its own code rather than being folded into a generic fetch failure.
 */
sealed class BlobError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * Transient state, not a durability loss: an asset visible in the sync feed whose original blob
     * has not been uploaded yet (staged uploads / awaiting-original).
     */
    data object PendingUpload : BlobError(
        code = "error.blob.pending_upload",
        detail = "The original photo hasn't been uploaded from its device yet.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<BlobError> =
            listOf(
                PendingUpload,
            )
    }
}
