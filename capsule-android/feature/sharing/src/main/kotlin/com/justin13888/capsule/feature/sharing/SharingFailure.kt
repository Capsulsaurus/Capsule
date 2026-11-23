package com.justin13888.capsule.feature.sharing

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.model.ErrorCode

/**
 * Why a sharing surface cannot show what it was asked for.
 *
 * Split into a [kind] and an optional [code] rather than carrying the exception, because the
 * screen has exactly two decisions to make and they are not the same decision. The kind picks
 * the remediation — "connect and retry" reads nothing like "try again" — while the code picks
 * the sentence, localized from the catalogue. Neither of them is ever an HTTP status: the
 * status says nothing a user can act on and two deployments may spell it differently.
 */
@Immutable
data class SharingFailure(
    /** Which remediation the screen should offer. */
    val kind: SharingFailureKind,
    /** The catalogue code to localize, or `null` for a purely local failure. */
    val code: ErrorCode? = null,
) {
    internal companion object {
        /**
         * Classifies whatever a repository threw.
         *
         * Offline is lifted out of the coded failures deliberately. The Local Gallery contract
         * makes having no server a *mode*, not an error, so a link list that cannot be
         * refreshed says so and keeps showing what it already has, rather than presenting the
         * same red state as a server that refused.
         */
        fun of(throwable: Throwable): SharingFailure =
            when (throwable) {
                is LocalError.Offline -> SharingFailure(SharingFailureKind.Offline)
                is CapsuleError -> SharingFailure(SharingFailureKind.Reported, throwable.code)
                else -> SharingFailure(SharingFailureKind.Unknown)
            }
    }
}

/** The remediation a [SharingFailure] calls for. */
enum class SharingFailureKind {
    /** No usable network path. The links already read stay on screen. */
    Offline,

    /** The server refused and named a reason the catalogue can localize. */
    Reported,

    /** Something no contract anticipated. The generic message is the honest answer. */
    Unknown,
}
