package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures walking the change feed.
 *
 * A rejected cursor is not a transient failure: it means the client's position in the feed is not
 * one this server will honour, so the client must re-establish it rather than retry.
 */
sealed class SyncError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * gRPC INVALID_ARGUMENT (threat-model invariant 22): the opaque sync cursor failed its server
     * MAC — forged, mutated, or lifted from another context.
     */
    data object CursorInvalid : SyncError(
        code = "error.sync.cursor_invalid",
        detail = "The sync session is out of date. Capsule will resync from the start.",
    )

    /**
     * gRPC UNAUTHENTICATED on the sync feed: the request carried no valid access token in the
     * `authorization` call metadata.
     */
    data object Unauthenticated : SyncError(
        code = "error.sync.unauthenticated",
        detail = "Please sign in again to continue syncing.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<SyncError> =
            listOf(
                CursorInvalid,
                Unauthenticated,
            )
    }
}
