package com.justin13888.capsule.feature.drops

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.model.ErrorCode

/**
 * Why a drops surface cannot show or do what it was asked.
 *
 * Split into a [kind] and an optional [code] rather than carrying the exception, because the
 * screen has two separate decisions to make: which remediation to offer, and which sentence
 * to print. Neither of them is ever an HTTP status — a status tells the user nothing they can
 * act on, and two deployments may spell the same condition differently.
 */
@Immutable
data class DropsFailure(
    /** Which remediation the screen should offer. */
    val kind: DropsFailureKind,
    /** The catalogue code to localize, or `null` for a purely local failure. */
    val code: ErrorCode? = null,
) {
    internal companion object {
        /**
         * Classifies whatever a repository threw.
         *
         * Offline is lifted out of the coded failures on purpose. Reviewing the inbox is a
         * read and keeps working with no server; only adopting and discarding need one, so
         * "you are offline" is a statement about which buttons work, not a failed screen.
         */
        fun of(throwable: Throwable): DropsFailure =
            when (throwable) {
                is LocalError.Offline -> DropsFailure(DropsFailureKind.Offline)
                is CapsuleError -> DropsFailure(DropsFailureKind.Reported, throwable.code)
                else -> DropsFailure(DropsFailureKind.Unknown)
            }
    }
}

/** The remediation a [DropsFailure] calls for. */
enum class DropsFailureKind {
    /** No usable network path. What is already in the inbox stays readable. */
    Offline,

    /** The server refused and named a reason the catalogue can localize. */
    Reported,

    /** Something no contract anticipated. The generic message is the honest answer. */
    Unknown,
}
