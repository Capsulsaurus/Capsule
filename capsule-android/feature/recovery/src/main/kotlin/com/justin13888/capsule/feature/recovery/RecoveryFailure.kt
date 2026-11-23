package com.justin13888.capsule.feature.recovery

import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.ErrorCode

/**
 * A failure one of these screens has to explain.
 *
 * Carries the stable `error.*` code rather than the exception, so a screen localizes a code
 * and never renders a transport status or an English detail string at the user. The code is
 * nullable because a purely local failure genuinely has none — and verification is entirely
 * local, so that is the common case here rather than an edge one.
 */
@JvmInline
value class RecoveryFailure(
    /** The catalogue code to localize, or `null` for a device-local failure. */
    val code: ErrorCode?,
) {
    internal companion object {
        /** Keeps only the part of [throwable] the user can act on. */
        fun of(throwable: Throwable): RecoveryFailure = RecoveryFailure((throwable as? CapsuleError)?.code)
    }
}
