package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures provisioning or resolving a container album.
 *
 * Both variants are deliberately uninformative about *why* an id was refused: the endpoint must
 * never reveal whether another account already holds a given album id.
 */
sealed class AlbumError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 400 on POST /v1/albums (album provisioning, S-C25): album_id is not a canonical lowercase
     * hyphenated UUID.
     */
    data object InvalidId : AlbumError(
        code = "error.album.invalid_id",
        detail = "This album could not be registered because its identifier is malformed.",
    )

    /**
     * HTTP 403 on POST /v1/albums (album provisioning, S-C25): the submitted album id cannot be
     * bound to the caller.
     */
    data object NotAvailable : AlbumError(
        code = "error.album.not_available",
        detail = "This album isn't available on your account.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<AlbumError> =
            listOf(
                InvalidId,
                NotAvailable,
            )
    }
}
