package com.justin13888.capsule.feature.recovery

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.RecoveryCadence
import java.time.Instant

/** Where the local check has got to. */
enum class VerifyPhase {
    /** Waiting for a passphrase. */
    Editing,

    /** Deriving and comparing, on this device. */
    Checking,

    /** It unwrapped the escrow. */
    Verified,

    /** It did not — and nothing is locked. */
    NotVerified,

    /** The check itself could not run. */
    Failed,
}

/**
 * Everything the verification screen draws, as one value.
 *
 * There is no attempt budget here and no lockout counter, and that is the design rather than
 * an omission: the check is a local derived-tag comparison against a cached blob, so it
 * creates no guessing oracle for anyone, and refusing further attempts would protect nothing
 * while stranding the one user it was meant to help.
 */
@Immutable
data class RecoveryVerifyUiState(
    /** What the user has typed. Held only while this screen is on top. */
    val secret: String = "",
    /** Whether the field is showing what was typed. */
    val revealed: Boolean = false,
    /** Where the check has got to. */
    val phase: VerifyPhase = VerifyPhase.Editing,
    /** How many times in a row the entered secret has not matched. */
    val consecutiveFailures: Int = 0,
    /** When the next prompt is due, as the cadence derives it. */
    val nextPromptAt: Instant? = null,
    /** The failure to explain, or `null`. */
    val failure: RecoveryFailure? = null,
    /** Whether the last attempt was refused for being empty. */
    val secretMissing: Boolean = false,
) {
    /** Whether the check can be run. */
    val canSubmit: Boolean get() = phase != VerifyPhase.Checking

    /** Whether enough attempts have missed that a fresh secret is the better answer. */
    val rewrapRecommended: Boolean get() = consecutiveFailures >= RecoveryCadence.FAILURES_BEFORE_REWRAP

    /** How many attempts the client makes before it offers to mint a new secret instead. */
    val failureLimit: Int get() = RecoveryCadence.FAILURES_BEFORE_REWRAP
}
