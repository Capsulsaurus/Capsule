package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures establishing or tearing down an authenticated session.
 *
 * Every credential failure answers identically, so the endpoint is not an oracle for which half of
 * a credential pair was wrong, or for whether an account exists at all.
 */
sealed class AuthError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * High-level error for HTTP 401 on login. Server sends code `error.auth.invalid_credentials`.
     */
    data object InvalidCredentials : AuthError(
        code = "error.auth.invalid_credentials",
        detail = "Invalid email or password.",
    )

    /**
     * High-level error for HTTP 429. Server sends code `error.auth.rate_limited`.
     */
    data object RateLimited : AuthError(
        code = "error.auth.rate_limited",
        detail = "Too many attempts. Please wait and try again.",
    )

    /**
     * HTTP 401 on POST /logout/all (slice S-C23): a proof was presented but did not verify — an
     * unknown, spent, or expired challenge; an account with no published device directory to anchor
     * the identity key; a key that does not sign that directory; or a bad signature.
     */
    data object RevokeProofInvalid : AuthError(
        code = "error.auth.revoke_proof_invalid",
        detail = "That didn't confirm as you. Nothing was logged out.",
    )

    /**
     * HTTP 401 on POST /logout/all (slice S-C23): no readable master-key proof was presented.
     */
    data object RevokeProofRequired : AuthError(
        code = "error.auth.revoke_proof_required",
        detail = "Confirm with your account key to log out of all devices.",
    )

    /**
     * High-level error for HTTP 409 on registration. Server sends code
     * `error.auth.user_already_exists`; clients localize this message.
     */
    data object UserAlreadyExists : AuthError(
        code = "error.auth.user_already_exists",
        detail = "An account with these details already exists.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<AuthError> =
            listOf(
                InvalidCredentials,
                RateLimited,
                RevokeProofInvalid,
                RevokeProofRequired,
                UserAlreadyExists,
            )
    }
}
