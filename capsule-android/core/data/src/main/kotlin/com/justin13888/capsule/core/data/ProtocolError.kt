package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Refusals from the universal protocol handshake.
 *
 * The one refusal a client cannot work around, and the one it must present as an update prompt
 * rather than as an error.
 */
sealed class ProtocolError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 426 from the universal protocol handshake (invariant 1): the client's X-Capsule-Protocol
     * date falls outside the server's accepted window.
     */
    data object VersionUnsupported : ProtocolError(
        code = "error.protocol.version_unsupported",
        detail = "This version of Capsule is no longer supported. Please update the app.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<ProtocolError> =
            listOf(
                VersionUnsupported,
            )
    }
}
