package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures adding a device to the user's directory.
 *
 * Enrollment adds a signing identity to the account, so its refusals are deliberately strict: rate
 * limits and single-use codes bound how many attempts an attacker gets.
 */
sealed class EnrollmentError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 404 on the enrollment relay channel (slice S-C7): the channel handle is unknown or
     * expired, so the server has nothing to relay between the two devices.
     */
    data object ChannelNotFound : EnrollmentError(
        code = "error.enrollment.channel_not_found",
        detail = "This device-add session has ended. Start again.",
    )

    /**
     * HTTP 404 on POST /devices/enroll/redeem (slice S-C7): the submitted enrollment code is
     * unknown, already redeemed, expired, or rate-limited.
     */
    data object CodeRefused : EnrollmentError(
        code = "error.enrollment.code_refused",
        detail = "That device code didn't work. Generate a new one and try again.",
    )

    /**
     * HTTP 403 on POST /devices/enroll (slice S-C7): the access token is not fresh enough to stand
     * in for the doc's fresh local device authorization.
     */
    data object LocalAuthRequired : EnrollmentError(
        code = "error.enrollment.local_auth_required",
        detail = "Confirm it's you on this device to add another device.",
    )

    /**
     * HTTP 429 on POST /devices/enroll (slice S-C7): the caller exceeded the per-user enrollment-
     * code issuance budget for the current window.
     */
    data object RateLimited : EnrollmentError(
        code = "error.enrollment.rate_limited",
        detail = "Too many device-add attempts. Please wait and try again.",
    )

    /**
     * HTTP 400 on the enrollment relay channel (slice S-C7): the request named an unknown direction
     * or carried a missing/oversized opaque payload.
     */
    data object RelayMalformed : EnrollmentError(
        code = "error.enrollment.relay_malformed",
        detail = "The device-add request could not be read.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<EnrollmentError> =
            listOf(
                ChannelNotFound,
                CodeRefused,
                LocalAuthRequired,
                RateLimited,
                RelayMalformed,
            )
    }
}
