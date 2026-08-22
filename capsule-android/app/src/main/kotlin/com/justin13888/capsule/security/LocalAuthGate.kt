package com.justin13888.capsule.security

import androidx.compose.runtime.Stable
import androidx.compose.runtime.mutableStateMapOf
import com.justin13888.capsule.core.model.GatedView
import com.justin13888.capsule.core.model.LocalAuthGrant
import java.time.Clock
import java.time.Instant

/**
 * What the platform prompt has to be able to do, and nothing more.
 *
 * A port rather than a direct `BiometricPrompt` call so the gate is testable: a Robolectric
 * test cannot satisfy a real biometric prompt, and a gate that can only be tested on a device
 * is a gate that stops being tested. It is also the seam for the biometric-then-credential
 * fallback the security requirement asks for — core sees only the outcome, never which
 * modality produced it.
 */
interface LocalAuthPrompt {
    /**
     * Raise the platform prompt for [view] and suspend until the user answers.
     *
     * @return true when the user authenticated. Cancellation, refusal and "no authenticator
     *   is enrolled" are all false: the gate's job is to open or not open, and a caller that
     *   branched on why would be inventing policy the platform already decided.
     */
    suspend fun authenticate(view: GatedView): Boolean
}

/**
 * Grants of fresh local authentication, one per gated view.
 *
 * Implements security requirement SR1: opening Recently Deleted or Hidden needs fresh local
 * auth, and one grant covers a short grace window — five minutes by default, from
 * [LocalAuthGrant.DEFAULT_WINDOW] — after which the view re-locks.
 *
 * Grants are held per view and never shared. That is not tidiness: the two views answer
 * different questions about what the user meant to keep out of sight, and unlocking the trash
 * to recover a photo must not also expose everything they deliberately hid.
 *
 * This is UX protection against a borrowed unlocked phone, not a cryptographic boundary — the
 * same bytes are reachable by anyone who defeats the platform sandbox. Saying so here matters
 * because a reader who assumed otherwise might skip real at-rest protection on its strength.
 *
 * @param clock injected so the grace window can be tested without sleeping for five minutes.
 */
@Stable
class LocalAuthGate(
    private val clock: Clock = Clock.systemUTC(),
) {
    private val grants = mutableStateMapOf<GatedView, LocalAuthGrant>()

    /**
     * Whether [view] may open right now without re-authenticating.
     *
     * A pure read: an expired grant is left in the map rather than evicted, because this is
     * called from composition and a write there would schedule a recomposition on every frame
     * that happened to straddle the expiry. Freshness is re-derived each call, so a stale
     * entry can never open anything.
     */
    fun isOpen(view: GatedView): Boolean = grants[view]?.isFreshAt(Instant.now(clock)) == true

    /** Record that the user just authenticated for [view]. */
    fun grant(view: GatedView) {
        grants[view] = LocalAuthGrant(view = view, grantedAt = Instant.now(clock))
    }

    /**
     * Drop every grant.
     *
     * Called when the app goes to the background: the grace window is about a user who is
     * still holding the phone, and backgrounding is the clearest signal that they are not.
     */
    fun revokeAll() {
        grants.clear()
    }
}
