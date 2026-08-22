package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures publishing or reading the signed device directory.
 *
 * The version conflict is the anti-rollback gate: a publish that does not strictly advance the
 * directory version is refused, so a server cannot roll back a revocation.
 */
sealed class DirectoryError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 400 on device-directory publish: the request body is not a decodable signed
     * DeviceDirectory document (bad CBOR, or a directory_version outside the representable range).
     */
    data object Malformed : DirectoryError(
        code = "error.directory.malformed",
        detail = "The device list could not be read.",
    )

    /**
     * HTTP 409 on device-directory publish (threat-model invariant 23): the submitted
     * DeviceDirectory's directory_version does not strictly advance the version already stored for
     * the user.
     */
    data object VersionConflict : DirectoryError(
        code = "error.directory.version_conflict",
        detail = "This device list is out of date. Capsule will refresh it before continuing.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<DirectoryError> =
            listOf(
                Malformed,
                VersionConflict,
            )
    }
}
