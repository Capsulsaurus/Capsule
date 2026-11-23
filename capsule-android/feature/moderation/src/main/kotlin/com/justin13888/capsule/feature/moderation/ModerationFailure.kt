package com.justin13888.capsule.feature.moderation

import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.ErrorCode

/**
 * A failure one of these screens has to explain.
 *
 * Carries the stable `error.*` code rather than the exception, so a screen localizes a code
 * and never renders a transport status or an English detail string at the user. The code is
 * nullable because a purely local failure genuinely has none, and minting one would be a
 * catalogue key that means nothing.
 */
@JvmInline
value class ModerationFailure(
    /** The catalogue code to localize, or `null` for a device-local failure. */
    val code: ErrorCode?,
) {
    internal companion object {
        /** Keeps only the part of [throwable] the user can act on. */
        fun of(throwable: Throwable): ModerationFailure = ModerationFailure((throwable as? CapsuleError)?.code)
    }
}
